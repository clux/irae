use crate::{estimate, Error, Kind, Result, Rollout, State, StatefulSummary};
use indicatif::{ProgressBar, ProgressStyle};
use kube::{
    core::{Expression, Selector},
    ResourceExt,
};
use std::time::Duration;
use tokio::time::sleep;
#[allow(unused_imports)] use tracing::{debug, error, info, trace, warn};

// ----------------------------------------------------------------------------
// indicatif tracker loop

/// Track the rollout of the main workload
///
/// This is currently designed to be called right after a kubectl apply
/// and may need modifications
pub async fn workload_rollout(r: &Rollout) -> Result<(bool, State)> {
    // 1. need to infer properties from the workload first to get information about how to track
    let params = r.infer_parameters().await?;
    // 2. use parameters to estimate how long to wait for an upgrade
    let waittime = estimate::wait_time(&params);
    // 3. Prepare state, selectors
    let poll_duration = std::time::Duration::from_millis(1000);
    let name = r.name.clone();
    let mut state = State {
        min_replicas: params.min_replicas, // TODO: maybe update during?
        hash: None,
        selector: Selector::default(),
    };
    // 4. Use found pod selector on workload to look for child objects
    let deployment_selector: Selector = params
        .selector
        .try_into()
        .map_err(|e| Error::KubeInvariant(format!("malformed label selector: {e}")))?;
    state.selector.extend(deployment_selector);

    // 5. Check if we need to actually need to do something first
    match r.status(&state).await {
        Ok(rr) => {
            if rr.ok {
                return Ok((true, state));
            } else {
                debug!("Ignoring rollout failure right after upgrade")
            }
        }
        Err(e) => warn!("Ignoring rollout failure right after upgrade: {}", e),
    };
    // Wait for the api server to accept the yaml
    sleep(poll_duration).await;

    // 6. Determine child objects for the rollout we are following
    // This is not always sound (multiple upgrades may clash with each other)
    // A smarter algorithm might change replicasets mid tracking to account for this.
    info!("Waiting {waittime}s for {name} to rollout (not ready yet)",);
    // TODO: handle unscheduleble?
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
        Kind::DaemonSet => unimplemented!(),
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
            Kind::DaemonSet => pb.set_prefix(h),   // TODO: test
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
            return Ok((true, state));
        }
    }
    Ok((false, state)) // timeout
}
