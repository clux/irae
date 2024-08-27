use crate::{Error, Kind, Result, Rollout, RolloutStrategy};
use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, StatefulSet};
use k8s_openapi::api::core::v1::PodSpec;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;

#[derive(Clone, Debug)]
pub struct Inference {
    /// Rollout Strategy
    pub strategy: Option<RolloutStrategy>,
    /// Label selectors used to find child resources (e.g. replicasets)
    pub selector: LabelSelector,
    /// Minimum number of replicas to wait for
    pub min_replicas: u32,
    /// Initial delay seconds for readiness probe if set
    pub initial_delay_seconds: Option<u32>,
}

impl Rollout {
    pub async fn infer_parameters(&self) -> Result<Inference> {
        let inference = match self.workload {
            Kind::Deployment => {
                let d = self.get_deploy().await?;
                Inference {
                    selector: find_deploy_selector(&d)
                        .ok_or_else(|| Error::KubeInvariant("no workload on deploy".to_string()))?,
                    min_replicas: find_deploy_replicas(&d)
                        .ok_or_else(|| Error::KubeInvariant("no replicas status".to_string()))?,
                    strategy: find_deploy_strategy(&d),
                    initial_delay_seconds: find_deploy_delay(&d),
                }
            }
            Kind::StatefulSet => {
                let sts = self.get_statefulset().await?;
                Inference {
                    selector: find_sts_selector(&sts)
                        .ok_or_else(|| Error::KubeInvariant("no selector on sts".to_string()))?,
                    min_replicas: find_sts_replicas(&sts)
                        .ok_or_else(|| Error::KubeInvariant("no replicas status".to_string()))?,
                    strategy: find_sts_strategy(&sts),
                    initial_delay_seconds: find_sts_delay(&sts),
                }
            }
            Kind::DaemonSet => {
                let ds = self.get_daemonset().await?;
                Inference {
                    selector: find_ds_selector(&ds)
                        .ok_or_else(|| Error::KubeInvariant("no selector on ds".to_string()))?,
                    min_replicas: find_ds_replicas(&ds)
                        .ok_or_else(|| Error::KubeInvariant("no replicas status".to_string()))?,
                    strategy: find_ds_strategy(&ds),
                    initial_delay_seconds: find_ds_delay(&ds),
                }
            }
        };
        Ok(inference)
    }
}

fn find_deploy_selector(d: &Deployment) -> Option<LabelSelector> {
    let spec = d.spec.as_ref()?;
    Some(spec.selector.clone())
}
fn find_sts_selector(sts: &StatefulSet) -> Option<LabelSelector> {
    let spec = sts.spec.as_ref()?;
    Some(spec.selector.clone())
}
fn find_ds_selector(sts: &DaemonSet) -> Option<LabelSelector> {
    let spec = sts.spec.as_ref()?;
    Some(spec.selector.clone())
}

fn find_deploy_strategy(d: &Deployment) -> Option<RolloutStrategy> {
    let spec = d.spec.as_ref()?;
    let native_strat = spec.strategy.as_ref()?.rolling_update.clone();
    Some(native_strat?.into())
}
fn find_sts_strategy(sts: &StatefulSet) -> Option<RolloutStrategy> {
    let spec = sts.spec.as_ref()?;
    let native_strat = spec.update_strategy.as_ref()?.rolling_update.clone();
    Some(native_strat?.into())
}
fn find_ds_strategy(ds: &DaemonSet) -> Option<RolloutStrategy> {
    let spec = ds.spec.as_ref()?;
    let native_strat = spec.update_strategy.as_ref()?.rolling_update.clone();
    Some(native_strat?.into())
}

fn find_deploy_replicas(d: &Deployment) -> Option<u32> {
    let spec = d.spec.as_ref()?;
    let status = d.status.as_ref()?;
    spec.replicas.or(status.replicas).map(|x| x.try_into().unwrap())
}
fn find_sts_replicas(d: &StatefulSet) -> Option<u32> {
    let spec = d.spec.as_ref()?;
    let status = d.status.as_ref()?;
    let replicas = spec.replicas.unwrap_or(status.replicas);
    Some(replicas.try_into().unwrap())
}
fn find_ds_replicas(d: &DaemonSet) -> Option<u32> {
    let status = d.status.as_ref()?;
    let replicas = status.desired_number_scheduled;
    Some(replicas.try_into().unwrap())
}

fn find_deploy_delay(d: &Deployment) -> Option<u32> {
    let spec = d.spec.as_ref()?;
    let tpl = spec.template.spec.as_ref()?;
    find_pod_delay(&tpl)
}
fn find_sts_delay(d: &StatefulSet) -> Option<u32> {
    let spec = d.spec.as_ref()?;
    let tpl = spec.template.spec.as_ref()?;
    find_pod_delay(&tpl)
}
fn find_ds_delay(d: &DaemonSet) -> Option<u32> {
    let spec = d.spec.as_ref()?;
    let tpl = spec.template.spec.as_ref()?;
    find_pod_delay(&tpl)
}
fn find_pod_delay(p: &PodSpec) -> Option<u32> {
    let mut max_delay = 0;
    for c in &p.containers {
        if let Some(rp) = &c.readiness_probe {
            if let Some(delay) = rp.initial_delay_seconds {
                max_delay = std::cmp::max(max_delay, delay.unsigned_abs());
            }
        }
    }
    Some(max_delay)
}
