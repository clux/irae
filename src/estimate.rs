use k8s_openapi::api::apps::v1::RollingUpdateDaemonSet as DsStrategy;
use k8s_openapi::api::apps::v1::RollingUpdateDeployment as DeployStrategy;
use k8s_openapi::api::apps::v1::RollingUpdateStatefulSetStrategy as StsStrategy;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
#[allow(unused_imports)]
use tracing::{debug, error, info, trace, warn};

/// Analogue of IntOrString specific to rollout parameters
#[derive(Debug, Clone)]
pub enum AvailabilityPolicy {
    Percentage(String),
    Unsigned(u32),
}

impl From<IntOrString> for AvailabilityPolicy {
    fn from(ios: IntOrString) -> Self {
        match ios {
            IntOrString::Int(i) => Self::Unsigned(i.unsigned_abs()), // integers always positive in this context
            IntOrString::String(p) => Self::Percentage(p),
        }
    }
}

// Kube has a weird hybrid type for this intstr.IntOrString: IntVal | StrVal
// if it's a string, then '[0-9]+%!' has to parse
impl AvailabilityPolicy {
    /// Figure out how many the availability policy refers to
    ///
    /// This multiplies the policy with num replicas and rounds up (for maxSurge)
    fn to_replicas_ceil(&self, replicas: u32) -> u32 {
        match self {
            AvailabilityPolicy::Percentage(percstr) => {
                let digits = percstr.chars().take_while(|ch| *ch != '%').collect::<String>();
                let surgeperc: u32 = digits.parse().unwrap(); // safe due to verify ^
                ((f64::from(replicas) * f64::from(surgeperc)) / 100.0).ceil() as u32
            }
            AvailabilityPolicy::Unsigned(u) => *u,
        }
    }

    /// Figure out how many the availability policy refers to
    ///
    /// This multiplies the policy with num replicas and rounds down (for maxUnavailable)
    fn to_replicas_floor(&self, replicas: u32) -> u32 {
        match self {
            AvailabilityPolicy::Percentage(percstr) => {
                let digits = percstr.chars().take_while(|ch| *ch != '%').collect::<String>();
                let surgeperc: u32 = digits.parse().unwrap(); // safe due to verify ^
                ((f64::from(replicas) * f64::from(surgeperc)) / 100.0).floor() as u32
            }
            AvailabilityPolicy::Unsigned(u) => *u,
        }
    }
}

// convert rollout strategies from k8s-openapi
impl From<DeployStrategy> for RolloutStrategy {
    fn from(ds: DeployStrategy) -> Self {
        Self {
            max_unavailable: ds.max_unavailable.map(Into::into),
            max_surge: ds.max_surge.map(Into::into),
        }
    }
}
impl From<DsStrategy> for RolloutStrategy {
    fn from(ds: DsStrategy) -> Self {
        Self {
            max_unavailable: ds.max_unavailable.map(Into::into),
            max_surge: ds.max_surge.map(Into::into),
        }
    }
}
impl From<StsStrategy> for RolloutStrategy {
    fn from(ds: StsStrategy) -> Self {
        Self {
            max_unavailable: ds.max_unavailable.map(Into::into),
            max_surge: Some(AvailabilityPolicy::Unsigned(0)), // no surge for sts
        }
    }
}

/// Configuration parameters for Deployment.spec.strategy.rollingUpdate
#[derive(Debug, Clone)]
pub struct RolloutStrategy {
    /// How many replicas or percentage of replicas that can be down during rolling-update
    pub max_unavailable: Option<AvailabilityPolicy>,
    /// Maximum number of pods that can be created over replicaCount
    pub max_surge: Option<AvailabilityPolicy>,
}

/// Implement Default that matches kubernetes
///
/// Both values defined in kube docs for deployment under .spec.strategy
/// https://kubernetes.io/docs/concepts/workloads/controllers/deployment/#writing-a-deployment-spec
impl Default for RolloutStrategy {
    fn default() -> Self {
        Self {
            max_unavailable: Some(AvailabilityPolicy::Percentage(25.to_string())),
            max_surge: Some(AvailabilityPolicy::Percentage(25.to_string())),
        }
    }
}

impl RolloutStrategy {
    /// Estimate how many cycles is needed to roll out a new version
    ///
    /// This is a bit arcane extrapolates from [rolling update documentation](https://kubernetes.io/docs/concepts/workloads/controllers/deployment/#max-unavailable)
    /// It needs to keep into account both values.
    pub fn rollout_iterations(&self, replicas: u32) -> u32 {
        let surge = if let Some(surge) = &self.max_surge {
            // surge is max number/percentage
            surge.to_replicas_ceil(replicas)
        } else {
            // default surge percentage is 25
            (f64::from(replicas * 25) / 100.0).ceil() as u32
        };
        let unavail = if let Some(unav) = &self.max_unavailable {
            // maxUnavailable is max number/percentage
            unav.to_replicas_floor(replicas)
        } else {
            (f64::from(replicas * 25) / 100.0).floor() as u32
        };
        // Work out how many iterations is needed assuming consistent rollout time
        // Often, this is not true, but it provides a good indication
        let mut newrs = 0;
        let mut oldrs = replicas; // keep track of for ease of following logic
        let mut iters = 0;
        trace!(
            "rollout iterations for {} replicas, surge={},unav={}",
            replicas,
            surge,
            unavail
        );
        while newrs < replicas {
            // kill from oldrs the difference in total if we are surging
            oldrs -= oldrs + newrs - replicas; // noop if surge == 0
                                               // terminate pods so we have at least maxUnavailable
            let total = newrs + oldrs;

            let unavail_safe = if total <= unavail { 0 } else { unavail };
            trace!(
                "oldrs{}, total is {}, unavail_safe: {}",
                oldrs,
                total,
                unavail_safe
            );
            oldrs -= std::cmp::min(oldrs, unavail_safe); // never integer overflow
                                                         // add new pods to cover and allow surging a little
            newrs += unavail_safe;
            newrs += surge;
            // after this iteration, assume we have rolled out newrs replicas
            // and we hve ~_oldrs remaining (ignoring <0 case)
            iters += 1;
            trace!("rollout iter {}: old={}, new={}", iters, oldrs, newrs);
        }
        trace!("rollout iters={}", iters);
        iters
    }

    pub fn rollout_iterations_default(replicas: u32) -> u32 {
        // default surge percentage is 25
        ((f64::from(replicas) * 25.0) / 100.0).ceil() as u32
    }
}

/// Information needed to calculate a semi-accurate wait time
#[derive(Default, Clone, Debug)]
pub struct WaitParams {
    /// Rolling Update Paramteres from the workload
    ///
    /// Used to analytically determine how many iterations is needed to roll out
    ///
    /// - k8s_openapi::api::apps::v1::RollingUpdateDeployment
    /// - k8s_openapi::api::apps::v1::RollingUpdateDaemonSet
    rolling_update: Option<RolloutStrategy>,
    /// Number of replicas to wait for
    min_replicas: u32,
    /// The image size in megabytes
    ///
    /// Defaults to 512MB as a guess
    /// Scales the pulltime estimate based on relative difference with default.
    image_size: Option<u32>,
    /// The number of seconds to wait for readiness probes
    ///
    /// Defaults to 30s as a guess
    initial_delay_seconds: Option<u32>,
}

/// Estimate how long to wait for a kube rolling upgrade
///
/// Was used by helm, now used by the internal upgrade wait time.
/// Elide this fn, by using 300s as a default wait time (as per helm upgrade)
pub fn estimate_wait_time(wp: &WaitParams) -> u32 {
    let size = wp.image_size.unwrap_or(512);
    // TODO: maybe expose parameters
    // 512 default => extra 90s wait, then 90s per half gig...
    let pulltime_est = std::cmp::max(60, ((f64::from(size) * 90.0) / 512.0) as u32);
    let ru = wp.rolling_update.clone().unwrap_or_default();
    let iterations = ru.rollout_iterations(wp.min_replicas); // precise if rollingupdate values are

    trace!("estimating wait for {iterations} cycle rollout: size={size} (est={pulltime_est})",);

    // how long each iteration needs to wait due to readinessProbe params.
    let delay_time_secs = wp.initial_delay_seconds.unwrap_or(30);

    // give it some leeway
    // leeway scales linearly with wait because we assume accuracy goes down..
    let delay_time = (f64::from(delay_time_secs) * 1.5).ceil() as u32;

    // Final formula: (how long to wait to poll + how long to pull) * num cycles
    (delay_time + pulltime_est) * iterations
}
