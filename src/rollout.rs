use crate::{version_label, Error, Kind, Result, Rollout};

use k8s_openapi::api::{
    apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet},
    core::v1::{Container, Pod, PodTemplateSpec},
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time as K8sTime;
use kube::{
    api::{ListParams, LogParams},
    core::{NamespaceResourceScope, ObjectList, Selector},
    Api, Resource, ResourceExt,
};
use semver::Version;
use serde::de::DeserializeOwned;
//use std::time::Instant;
//use time::{ext::InstantExt, Duration};
use time::Duration;
#[allow(unused_imports)]
use tracing::{debug, error, info, warn};

// helpers to do kube api queries
impl Rollout {
    fn ns<K>(&self) -> Api<K>
    where
        K: Resource<Scope = NamespaceResourceScope, DynamicType = ()> + Clone + DeserializeOwned,
    {
        if let Some(ns) = &self.namespace {
            Api::namespaced(self.client.clone(), ns)
        } else {
            Api::default_namespaced(self.client.clone())
        }
    }
    /// Determine the currently leading replicaset
    ///
    /// Use standard label app.kubernetes.io/version to determine replicaset to track
    /// This is flawed in cases of rollbacks (where older versions' sets may be reclaimed)
    /// ..but need a more dedicated label to target otherwise
    pub async fn get_highest_version_replicaset(&self, selector: &Selector) -> Result<Option<ReplicaSet>> {
        // NB: replicaset selectors are based on the deployment selectors with an extra template hash
        let lp = ListParams::default().labels_from(&selector);
        let sets = self.ns().list(&lp).await.map_err(Error::Kube)?;
        let mut max_ver = semver::Version::new(0, 0, 0);
        let mut best = None;
        for rs in sets {
            let v = version_label(&rs)?;
            if v > max_ver {
                max_ver = v;
                best = Some(rs)
            }
        }
        Ok(best)
    }
    pub async fn get_rs(&self, selector: &Selector) -> Result<Option<ReplicaSet>> {
        let lp = ListParams::default().labels_from(&selector);
        let rs = self.ns().list(&lp).await.map_err(Error::Kube)?;
        assert_eq!(rs.items.len(), 1, "only one matching replicaset candidate");
        Ok(rs.items.first().cloned())
    }

    pub async fn get_pods(&self, selector: &Selector) -> Result<ObjectList<Pod>> {
        let lp = ListParams::default().labels_from(&selector);
        let pods = self.ns().list(&lp).await.map_err(Error::Kube)?;
        Ok(pods)
    }

    pub async fn get_deploy(&self) -> Result<Deployment> {
        let deploy = self.ns().get(&self.name).await.map_err(Error::Kube)?;
        Ok(deploy)
    }
    pub async fn get_statefulset(&self) -> Result<StatefulSet> {
        let sts = self.ns().get(&self.name).await.map_err(Error::Kube)?;
        Ok(sts)
    }
    pub async fn get_daemonset(&self) -> Result<DaemonSet> {
        let sts = self.ns().get(&self.name).await.map_err(Error::Kube)?;
        Ok(sts)
    }

    pub async fn get_pod_logs(&self, podname: &str) -> Result<String> {
        let lp = LogParams {
            tail_lines: Some(30),
            container: Some(self.name.to_string()),
            ..Default::default()
        };
        let logs = self.ns::<Pod>().logs(podname, &lp).await.map_err(Error::Kube)?;
        Ok(logs)
    }
}

// ----------------------------------------------------------------------------
// rollout tracking

/// The main output from `Track::status`
///
/// Provides a single snapshot from a point in time during a rollout of how far along we are.
/// Consumers should poll for this periodically and update states accordingly.
#[derive(Debug)]
pub struct Outcome {
    /// How far along the rollout we are
    pub progress: u32,
    /// Target we want to wait for
    pub expected: u32,
    /// Human readable progress indication (excluding counts)
    ///
    /// For deployments this comes from the Progressing condition.
    /// For statefulsets it is synthetic.
    pub message: Option<String>,
    /// Whether rollout completed and we should stop polling
    pub ok: bool,
}

#[derive(Debug, Clone)]
pub struct State {
    /// Template hash identifying child objects (such as replicasets)
    pub hash: Option<String>,
    /// Replica count to track
    pub min_replicas: u32,
    /// Moving selector to track (sometimes targets change before finishing)
    pub selector: Selector,
}

impl Rollout {
    /// Track a rollout and retun its current `Outcome`
    pub async fn status(&self, state: &State) -> Result<Outcome> {
        match self.workload {
            Kind::Deployment => rollout_status_deploy(self, state).await,
            Kind::StatefulSet => rollout_status_statefulset(self, state).await,
            Kind::DaemonSet => rollout_status_daemonset(self, state).await,
        }
    }
}

async fn rollout_status_deploy(r: &Rollout, state: &State) -> Result<Outcome> {
    // Get root data from Deployment status
    let deploy = r.get_deploy().await?;
    let name = deploy.name_any();
    let d = DeploySummary::try_from(deploy)?;
    debug!("{}: {:?}", r.name, d);
    // Wait for at least the minimum number...

    let mut accurate_progress = None; // accurate progress number
    let mut minimum = state.min_replicas; // minimum replicas we wait for
    if state.hash.is_some() {
        // Infer from pinned ReplicaSet status (that was latest during apply)
        if let Some(rs) = r.get_rs(&state.selector).await? {
            let r = ReplicaSetSummary::try_from(rs)?;
            debug!("{name}: {r:?}");
            accurate_progress = Some(r.ready);
            // rs might have scaled it up during rollout
            minimum = std::cmp::max(minimum, r.replicas.try_into().unwrap_or(0));
        }
    }

    // Decide whether to stop polling - did the upgrade pass?
    let ok = if let Some(acc) = accurate_progress {
        // Replicaset is scaled to our minimum, and all ready
        // NB: k8s >= 1.15 we use `d.new_replicas_available`
        // as a better required check
        acc == i32::try_from(minimum).expect("min number of replicas should have been within bounds of a i32")
    // NB: This last && enforces the progress downscaling at the end of fn
    } else {
        // FALLBACK (never seems to really happen): count from deployment only
        // Need to at least have as many ready as expected
        // ...it needs to have been scaled to the correct minimum
        // ...and, either we have the explicit progress done, or all unavailables are gone
        // The last condition (which increases waiting time) is necessary because:
        // deployment summary aggregates up the total number of ready pods
        // so we won't really know we're done or not unless we got the go-ahead
        // (i.e. d.new_replicas_available in k8s >= 1.15),
        // or all the unavailable pods have been killed (indicating total completeness)
        d.ready == d.replicas
            && d.ready
                >= minimum
                    .try_into()
                    .expect("min number of replicas should have been within bounds of a i32")
            && (d.new_replicas_available || d.unavailable <= 0)
    };

    //  What to tell our progress bar:
    let progress: i32 = match accurate_progress {
        // 99% case: the number from our accurately matched replicaset:
        Some(p) => p,

        // Otherwise estimate based on deployment.status data
        // Slightly weird data because of replicasets worth of data is here..
        // There might be more than one deployment in progress, all of which surge..
        None => std::cmp::max(0, d.ready - d.unavailable),
    };
    Ok(Outcome {
        progress: progress
            .try_into()
            .map_err(|_e| Error::KubeInvariant("progress >= 0".to_string()))?,
        expected: minimum,
        message: d.message,
        ok,
    })
}

async fn rollout_status_statefulset(r: &Rollout, state: &State) -> Result<Outcome> {
    let ss = r.get_statefulset().await?;
    let s = StatefulSummary::try_from(ss)?;
    let minimum = state.min_replicas;

    let ok = s.updated_replicas
        >= i32::try_from(minimum).expect("min number of replicas should have been within bounds of a i32")
        && s.updated_replicas == s.ready
        && s.update_revision == state.hash;
    let message = if ok {
        None
    } else {
        Some("Statefulset update in progress".to_string())
    };

    // NB: Progress is slightly optimistic because updated_replicas increment
    // as soon as the old replica is replaced, not when it's ready.
    // (we can't use ready_replicas because that counts the sum of old + new)
    // But this is OK. If it gets to 3/3 then at least 2 rolled out successfully,
    // and the third was started. The only other way of getting around that
    // would be tracking the pods with the new hash directly..

    // Note that while progressbar is optimistic, it's not marked as ok (done)
    // until the new revision is changed over (when sts controller thinks it's done)
    // So this is a progressbar only inconsistency.
    Ok(Outcome {
        progress: std::cmp::max(0, s.updated_replicas)
            .try_into()
            .expect("sts.updated_replicas >= 0"),
        expected: minimum,
        message,
        ok,
    })
}

// daemonset experimental
async fn rollout_status_daemonset(r: &Rollout, state: &State) -> Result<Outcome> {
    let ds = r.get_daemonset().await?;
    let s = DaemonSummary::try_from(ds)?;
    let minimum = state.min_replicas;

    let ok = s.desired
        >= i32::try_from(minimum).expect("min number of replicas should have been within bounds of a i32")
        && Some(s.desired) == s.updated;
    let message = if ok {
        None
    } else {
        Some("Daemonset update in progress".to_string())
    };
    Ok(Outcome {
        progress: std::cmp::max(0, s.updated.unwrap_or(s.ready))
            .try_into()
            .expect("sts.updated_replicas >= 0"),
        expected: minimum,
        message,
        ok,
    })
}

// ----------------------------------------------------------------------------
// misc formatting helpers

fn short_ver(ver: &str) -> String {
    if Version::parse(ver).is_err() && ver.len() == 40 {
        // only abbreviate versions that are not semver and 40 chars (git shas)
        ver[..8].to_string()
    } else {
        ver.to_string()
    }
}

fn format_duration(dur: Duration) -> String {
    let days = dur.whole_days();
    let hours = dur.whole_hours();
    let mins = dur.whole_minutes();
    if days > 0 {
        format!("{}d", days)
    } else if hours > 0 {
        format!("{}h", hours)
    } else {
        format!("{}m", mins)
    }
}

// ----------------------------------------------------------------------------
// misc version extraction helpers

fn extract_container<'a>(containers: &'a [Container], request: Option<&'a String>) -> Option<&'a Container> {
    let mut app_container = None;
    if let Some(specified) = request {
        app_container = containers.iter().find(|p| p.name == *specified);
    }
    let main_container = if let Some(appc) = app_container {
        appc
    } else {
        &containers[0]
    };
    Some(main_container)
}

fn default_container(pod: &Pod) -> Option<&Container> {
    let annotations = pod.annotations();
    let default_container = annotations.get("kubectl.kubernetes.io/default-container");
    if let Some(spec) = &pod.spec {
        extract_container(&spec.containers, default_container)
    } else {
        None
    }
}
fn find_default_in_rs(rs: &PodTemplateSpec) -> Option<String> {
    let meta = rs.metadata.as_ref()?;
    let annotations = meta.annotations.clone().unwrap_or_default();
    annotations
        .get("kubectl.kubernetes.io/default-container")
        .map(String::from)
}

// ----------------------------------------------------------------------------
// pod inspection - currently unused

/// A summary of a Pod's status
#[derive(Debug)]
pub struct PodSummary {
    /// Name of the pod inspected
    pub name: String,
    /// Age since the pod's creationTimestamp
    pub age: Duration,
    /// Phase from the status object
    pub phase: Option<String>,
    /// Number of running containers
    pub running: i32,
    /// Total number of containers
    pub containers: u32,
    /// Max number of restarts across containers
    pub restarts: i32,
    /// Version tag seen in image of main container
    pub version: Option<String>,
}

impl TryFrom<Pod> for PodSummary {
    type Error = Error;

    /// Helper to convert the openapi Pod to the useful info
    fn try_from(pod: Pod) -> Result<PodSummary> {
        let name = pod.name_any();
        let ts = pod
            .creation_timestamp()
            .unwrap_or(K8sTime(chrono::DateTime::<chrono::Utc>::MIN_UTC))
            .0;
        // TODO: we can't range error here can we?
        let age_std = chrono::Utc::now().signed_duration_since(ts).to_std().unwrap();
        let age = time::Duration::try_from(age_std).unwrap();

        let mut running = 0;
        let mut containers = 0;
        let mut restarts = 0;
        let mut phase = None;
        if let Some(status) = &pod.status {
            phase = status.phase.clone();
            for s in status.container_statuses.clone().unwrap_or_default() {
                running += if s.ready { 1 } else { 0 };
                containers += 1;
                restarts = std::cmp::max(restarts, s.restart_count);
            }
        }
        let mut version = None;
        if let Some(main_container) = default_container(&pod) {
            version = Some(short_ver(
                main_container
                    .image
                    .as_ref()
                    .unwrap()
                    .split(':')
                    .collect::<Vec<_>>()[1],
            ))
        };
        Ok(PodSummary {
            name,
            age,
            phase,
            version,
            running,
            containers,
            restarts,
        })
    }
}

// ----------------------------------------------------------------------------
// replicaset inspection

/// A summary of a ReplicaSet's status
#[derive(Debug)]
pub struct ReplicaSetSummary {
    pub hash: String,
    pub version: String,
    pub replicas: i32,
    pub ready: i32,
}

impl TryFrom<ReplicaSet> for ReplicaSetSummary {
    type Error = Error;

    /// Helper to convert the openapi ReplicaSet to the useful info
    fn try_from(rs: ReplicaSet) -> Result<ReplicaSetSummary> {
        let Some(status) = rs.status.clone() else {
            Err(Error::KubeInvariant(
                "Missing replicaset status object".to_string(),
            ))?
        };
        let name = rs.name_any();
        let replicas = status.replicas;
        let ready = status.ready_replicas.unwrap_or(0);
        let mut ver = None;
        if let Some(spec) = &rs.spec {
            if let Some(tpl) = &spec.template {
                if let Some(podspec) = &tpl.spec {
                    let default_container = find_default_in_rs(tpl);
                    if let Some(main) = extract_container(&podspec.containers, default_container.as_ref()) {
                        let image = main.image.clone().unwrap_or(":".to_string());
                        let tag = image.split(':').collect::<Vec<_>>()[1];
                        ver = Some(short_ver(tag));
                    }
                }
            }
        };
        let version = ver.unwrap_or_else(|| "unknown version".to_string());
        let hash = match rs.labels().get("pod-template-hash") {
            Some(h) => h.to_owned(),
            None => Err(Error::KubeInvariant(format!(
                "Need pod-template-hash from replicaset for {name}"
            )))?,
        };
        Ok(ReplicaSetSummary {
            hash,
            version,
            replicas,
            ready,
        })
    }
}

// ----------------------------------------------------------------------------
// deployment inspection

/// A summary of a Deployment's status
#[derive(Debug)]
pub struct DeploySummary {
    pub replicas: i32,
    pub unavailable: i32,
    pub ready: i32,
    pub new_replicas_available: bool,
    pub message: Option<String>,
}

impl TryFrom<Deployment> for DeploySummary {
    type Error = Error;

    /// Helper to convert the openapi Deployment to the useful info
    fn try_from(d: Deployment) -> Result<DeploySummary> {
        let Some(status) = d.status else {
            return Err(Error::KubeInvariant("Missing deployment status".to_string()));
        };

        // Sometimes kube tells us in an obscure way that the rollout is done:
        let mut message = None;
        let mut new_replicas_available = false;
        if let Some(conds) = status.conditions {
            // This is a shortcut that works in kubernetes >=1.15
            if let Some(pcond) = conds.iter().find(|c| c.type_ == "Progressing") {
                if let Some(reason) = &pcond.reason {
                    message = pcond.message.clone();
                    if reason == "NewReplicaSetAvailable" {
                        new_replicas_available = true;
                    }
                }
            }
        }
        Ok(DeploySummary {
            ready: status.ready_replicas.unwrap_or(0),
            unavailable: status.unavailable_replicas.unwrap_or(0),
            replicas: status.replicas.unwrap_or(0),
            message,
            new_replicas_available,
        })
    }
}

// ----------------------------------------------------------------------------
// statefulset inspection

/// A summary of a Statefulset's status
pub struct StatefulSummary {
    pub replicas: i32,
    pub ready: i32,
    pub current_revision: Option<String>,
    pub current_replicas: i32,
    pub update_revision: Option<String>,
    pub updated_replicas: i32,
}

impl TryFrom<StatefulSet> for StatefulSummary {
    type Error = Error;

    /// Helper to convert the openapi Statefulset to the useful info
    fn try_from(d: StatefulSet) -> Result<StatefulSummary> {
        let Some(status) = d.status else {
            Err(Error::KubeInvariant("Missing statefulset status".to_string()))?
        };
        // NB: No good message in statefulset conditions.. need to look at events to get one
        Ok(StatefulSummary {
            ready: status.ready_replicas.unwrap_or(0),
            replicas: status.replicas,
            current_revision: status.current_revision,
            current_replicas: status.current_replicas.unwrap_or(0),
            update_revision: status.update_revision,
            updated_replicas: status.updated_replicas.unwrap_or(0),
        })
    }
}

// ----------------------------------------------------------------------------
// daemonset inspection

/// A summary of a Daemonset's status
pub struct DaemonSummary {
    pub ready: i32,
    pub desired: i32,
    pub updated: Option<i32>,
}

impl TryFrom<DaemonSet> for DaemonSummary {
    type Error = Error;

    /// Helper to convert the openapi Statefulset to the useful info
    fn try_from(d: DaemonSet) -> Result<DaemonSummary> {
        let Some(status) = d.status else {
            Err(Error::KubeInvariant("Missing statefulset status".to_string()))?
        };
        // NB: No good message in statefulset conditions.. need to look at events to get one
        Ok(DaemonSummary {
            ready: status.number_ready,
            desired: status.desired_number_scheduled,
            updated: status.updated_number_scheduled,
        })
    }
}
