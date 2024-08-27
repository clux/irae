use kube::{Resource, ResourceExt};
use semver::Version;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("SerializationError: {0}")]
    Serialization(#[source] serde_json::Error),

    #[error("Kube Error: {0}")]
    Kube(#[source] kube::Error),

    #[error("Time Error: {0}")]
    Time(#[source] time::Error),

    #[error("IllegalDocument")]
    IllegalDocument,

    #[error("Non-semver app.kubernetes.io/version: {0}")]
    NonSemverVersion(String),

    #[error("K8s Invariant Error: {0}")]
    KubeInvariant(String),
}
pub type Result<T, E = Error> = std::result::Result<T, E>;
#[macro_export]
macro_rules! bail {
    ($msg:literal $(,)?) => {
        return Err($crate::Error::KubeInvariant($msg.to_string()))
    };
}

impl Error {
    pub fn metric_label(&self) -> String {
        format!("{self:?}").to_lowercase()
    }
}

// mod debug;
mod rollout;
pub use rollout::{DeploySummary, State, StatefulSummary};
mod estimate;
pub use estimate::RolloutStrategy;
mod infer;
#[cfg(feature = "term")]
pub mod term;

pub fn version_label<K: Resource>(k: &K) -> Result<Version> {
    if let Some(v) = k.labels().get("app.kubernetes.io/version") {
        let sem = Version::parse(v.as_ref()).map_err(|e| Error::NonSemverVersion(format!("{v}: {e}")))?;
        Ok(sem)
    } else {
        Err(Error::NonSemverVersion(format!("missing label")))
    }
}

/// A workload rollout to track with minimal user information
#[derive(Clone)]
pub struct Rollout {
    /// The workload name to be tracked
    pub name: String,
    /// The namespace of workload (if different than context namespace)
    pub namespace: Option<String>,
    /// The type of workload it is
    pub workload: Kind,
    /// Kubernetes interface
    pub client: kube::Client,
}
/// Support kinds to track rollouts for
#[derive(Clone, Debug)]
pub enum Kind {
    Deployment,
    StatefulSet,
    //DaemonSet
    //Kustomization
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {}
}
