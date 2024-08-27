use crate::{Error, Kind, Result, Rollout, RolloutStrategy};
use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet, StatefulSet},
    core::v1::{Container, Pod, PodTemplateSpec},
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;

#[derive(Clone, Debug)]
pub struct Inference {
    /// Rollout Strategy
    pub strategy: Option<RolloutStrategy>,
    /// Label selectors used to find child resources (e.g. replicasets)
    pub selector: LabelSelector,
    /// Minimum number of replicas to wait for
    /// TODO: should this be updated as we are tracking rollout?
    pub min_replicas: Option<u32>,
}

impl Rollout {
    pub async fn infer_parameters(&self) -> Result<Inference> {
        let inference = match self.workload {
            Kind::Deployment => {
                let d = self.get_deploy().await?;
                Inference {
                    selector: find_deploy_selector(&d)
                        .ok_or_else(|| Error::KubeInvariant("no workload on deploy".to_string()))?,
                    min_replicas: find_deploy_replicas(&d),
                    strategy: find_deploy_strategy(&d),
                }
            }
            Kind::StatefulSet => {
                let sts = self.get_statefulset().await?;
                Inference {
                    selector: find_sts_selector(&sts)
                        .ok_or_else(|| Error::KubeInvariant("no selector on sts".to_string()))?,
                    min_replicas: find_sts_replicas(&sts),
                    strategy: find_sts_strategy(&sts),
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

fn find_deploy_replicas(d: &Deployment) -> Option<u32> {
    let spec = d.spec.as_ref()?;
    Some(spec.replicas?.try_into().unwrap())
}
fn find_sts_replicas(d: &StatefulSet) -> Option<u32> {
    let spec = d.spec.as_ref()?;
    Some(spec.replicas?.try_into().unwrap())
}
