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
mod term;

/// A workload rollout to track
#[derive(Clone)]
pub struct Rollout {
    /// The workload name to be tracked
    pub name: String,
    /// The namespace it lives in
    pub namespace: String,
    /// The type of workload it is
    pub workload: String,
    /// The minimal amount of replicas to wait for
    pub min_replicas: u32, // replicaCount or hpa.minReplicas if autoscaling
    /// Kubernetes interface
    pub client: kube::Client,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {}
}
