//! debug rollout failures for potential reasons
use crate::{
    rollout::{PodSummary, ReplicaSetSummary},
    Error, Kind, Result, Rollout, State,
};

use k8s_openapi::api::core::v1::Pod;
use kube::core::ObjectList;
#[allow(unused_imports)] use tracing::{debug, error, info, warn};

impl Rollout {
    /// Debug why a workload is in the state it is in
    pub async fn debug(&self, state: &State) -> Result<()> {
        match self.workload {
            Kind::Deployment => debug_deployment(self, state).await,
            Kind::StatefulSet => debug_statefulset(self, state).await,
            Kind::DaemonSet => unimplemented!(),
        }
    }
}

/// Debug a deployment
///
/// Finds active replicaset (with pods in them)
/// Debugs the pods in each replicaset
/// Tails the logs from each broken pod
async fn debug_deployment(r: &Rollout, state: &State) -> Result<()> {
    // NB: this can technically loop over all replicasets with non-zero replicas
    // but the output would be confusing, better to stick with the one we tracked
    let Some(rs) = r.get_rs(&state.selector).await? else {
        return Ok(());
    };
    let summary = ReplicaSetSummary::try_from(rs)?;
    if summary.replicas == 0 {
        return Ok(());
    }
    info!(
        "{} Pod ReplicaSet {} running {}",
        summary.replicas, summary.hash, summary.version
    );
    let pods = r.get_pods(&state.selector).await?;
    info!("Replicaset contains:");
    debug_pods(r, pods).await?;
    Ok(())
}

async fn debug_statefulset(r: &Rollout, state: &State) -> Result<()> {
    // For now, just list the pods as if there were no replicaset to worry about
    let pods = r.get_pods(&state.selector).await?;
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
