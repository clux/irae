use crate::{DeploySummary, Kind, Rollout, State, StatefulSummary};
use crate::{Error, Result};
use indicatif::{ProgressBar, ProgressStyle};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::core::{Expression, Selector};
use kube::{Resource, ResourceExt};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info, trace, warn};

// ----------------------------------------------------------------------------
// indicatif tracker loop

/// Track the rollout of the main workload
pub async fn workload_rollout(r: &Rollout) -> Result<bool> {
    // 1. need to infer properties from the workload first to get information about how to track
    let params = r.infer_parameters().await?;
    // still need min replicas for cycles? if so then we also need to look at HPAs..
    // probabl enough these days to JUST Look at readyReplicas / updatedReplicas in .status
    // NEED: properties for wait time, selector for replicaset identification
    let waittime = 300; // TODO: hook up estimate_wait_time from estimate
    let poll_duration = std::time::Duration::from_millis(1000);
    let name = r.name.clone();
    let mut state = State {
        min_replicas: params.min_replicas.unwrap_or(2), // TODO: maybe update during?
        hash: None,
        selector: Selector::default(),
    };
    let deployment_selector: Selector = params
        .selector
        .try_into()
        .map_err(|e| Error::KubeInvariant(format!("malformed label selector: {e}")))?;
    state.selector.extend(deployment_selector);

    match r.status(&state).await {
        Ok(rr) => {
            if rr.ok {
                return Ok(true);
            } else {
                debug!("Ignoring rollout failure right after upgrade")
            }
        }
        Err(e) => warn!("Ignoring rollout failure right after upgrade: {}", e),
    };

    sleep(poll_duration).await;
    // TODO: Don't count until image has been pulled + handle unscheduleble
    info!("Waiting {waittime}s for {name} to rollout (not ready yet)",);
    match r.workload {
        Kind::Deployment => {
            // Attempt to find an owning RS hash to track
            if let Some(rs) = r.get_highest_version_replicaset(&state.selector).await? {
                if let Some(h) = rs.labels().get("pod-template-hash") {
                    debug!("Tracking replicaset {}", h);
                    let expr = Expression::Equal("pod-template-hash".into(), h.clone());
                    state.hash = Some(h.clone());
                    state.selector.extend(expr);
                }
            }
        }
        Kind::StatefulSet => {
            // Attempt to find an owning revesion hash to track
            let sts = r.get_statefulset().await?;
            let summary = StatefulSummary::try_from(sts)?;
            if let Some(ur) = summary.update_revision {
                debug!("Tracking statefulset {:?} for {}", ur, r.name);
                state.hash = Some(ur);
            }
        }
    }

    let pb = ProgressBar::new(state.min_replicas as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("> {bar:40.green/black} {prefix} {pos}/{len} ({elapsed}) {msg}")
            .expect("valid template string"),
    );
    //pb.set_draw_delta(1); removed
    if let Some(h) = state.hash.clone() {
        match r.workload {
            Kind::Deployment => pb.set_prefix(format!("{name}-{h}")),
            Kind::StatefulSet => pb.set_prefix(h), // statefulset hash already prefixes name
        }
    } else {
        pb.set_prefix(name);
    }

    for i in 1..20 {
        trace!("poll iteration {}", i);
        let mut waited = 0;
        // sleep until 1/20th of estimated upgrade time and poll for status
        while waited < waittime / 20 {
            waited += 1;
            trace!("sleep 1s (waited {})", waited);
            sleep(Duration::from_secs(1)).await;
        }
        let rr = r.status(&state).await?;
        debug!("RR: {:?}", rr);
        if let Some(msg) = rr.message {
            pb.set_message(msg);
        }
        pb.set_length(rr.expected.into()); // sometimes a replicaset resizes
        pb.set_position(rr.progress.into());
        if rr.ok {
            pb.finish();
            return Ok(true);
        }
    }
    Ok(false) // timeout
}
