//! debug rollout failures for potential reasons
use crate::Rollout;
use crate::{Error, Result};

use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet, StatefulSet},
    core::v1::{Container, Pod, PodTemplateSpec},
};
use kube::core::{NamespaceResourceScope, ObjectList};
use kube::{api::ListParams, Api, Resource, ResourceExt};
use tracing::{debug, error, info, warn};

impl Rollout {
    /// Debug why a workload is in the state it is in
    pub async fn debug(&self) -> Result<()> {
        match self.workload.as_ref() {
            "Deployment" => debug_deployment(self).await,
            "Statefulset" => debug_statefulset(self).await,
            _ => Ok(()),
        }
    }
}

/// Debug a deployment
///
/// Finds active replicasets (with pods in them)
/// Debugs the pods in each replicaset
/// Tails the logs from each broken pod
async fn debug_deployment(r: &Rollout) -> Result<()> {
    for rs in r.get_rs_by_app().await? {
        if let Ok(r) = ReplicaSetSummary::try_from(rs) {
            // NB: ^ ignore replicasets that didn't parse
            if r.replicas == 0 {
                continue; // also ignore empty ones..
            }
            info!("{} Pod ReplicaSet {} running {}", r.replicas, r.hash, r.version);
            let pods = r.get_pods_by_template_hash(&r.hash).await?;
            info!("Replicaset contains:");
            debug_pods(r, pods).await?;
        }
    }
    Ok(())
}

async fn debug_statefulset(r: &Rollout) -> Result<()> {
    // For now, just list the pods as if there were no replicaset to worry about
    let pods = r.get_pods().await?;
    //debug!("Statefulset contains: {pods:?}");
    debug_pods(r, pods).await?;
    Ok(())
}

async fn debug_pods(r: &Rollout, pods: ObjectList<Pod>) -> Result<()> {
    for pod in pods {
        let podstate = PodSummary::try_from(pod)?;
        println!("{:?}", podstate);
        if podstate.running != podstate.containers as i32 {
            info!(
                "Fetching logs from non-ready main container in pod: {}",
                podstate.name
            );
            match r.get_pod_logs(&podstate.name).await {
                Ok(logs) => {
                    warn!("Last 30 log lines:");
                    println!("{}", logs)
                }
                Err(e) => warn!("Failed to get logs from {}: {}", podstate.name, e),
            }
        }
    }
    Ok(())
}
