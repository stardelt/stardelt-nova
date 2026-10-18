//! A read-only view of the workloads running in Nova's namespace.
//!
//! Nova is the viewport: what the platform runs is a fact of the cluster, not of a
//! config file. These images come from the live Deployment/StatefulSet specs, so a
//! digest pin shows up as the digest rather than as the tag somebody meant to
//! deploy. Nova needs no write access — one Namespaced Role granting `get`/`list`
//! on `apps` is the whole permission surface (see `manifests/nova-deployment.yaml`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{Json, extract::State};
use k8s_openapi::api::apps::v1::{Deployment, StatefulSet};
use k8s_openapi::api::core::v1::PodTemplateSpec;
use kube::{
    Client,
    api::{Api, ListParams},
};
use serde::Serialize;
use tokio::sync::RwLock;

use crate::AppState;

/// One workload in the namespace, flattened to what a human wants to see.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Workload {
    pub name: String,
    /// Deployment | StatefulSet
    pub kind: String,
    /// Image reference as the cluster has it — tag or digest, verbatim.
    pub image: String,
    pub ready: i32,
    pub desired: i32,
}

/// The namespace Nova is deployed into, and reads.
pub fn namespace() -> String {
    std::env::var("POD_NAMESPACE")
        .or_else(|_| std::env::var("NOVA_NAMESPACE"))
        .unwrap_or_else(|_| "stardelt".into())
}

/// Snapshot cache. The UI polls `/api/apps` every few seconds; the API server
/// should not pay for that, and a 15 s stale image list is invisible to a human.
#[derive(Default)]
pub struct WorkloadCache {
    inner: RwLock<Option<(Instant, Vec<Workload>)>>,
}

const TTL: Duration = Duration::from_secs(15);

impl WorkloadCache {
    /// A cached snapshot of the namespace. Returns the last known good list when the
    /// cluster is temporarily unreachable, or an empty list when there is no client
    /// (demo mode, local run).
    pub async fn snapshot(&self, client: Option<&Client>, demo: bool) -> Vec<Workload> {
        if demo {
            return demo_workloads();
        }
        let Some(client) = client else {
            return Vec::new();
        };
        if let Some((at, cached)) = self.inner.read().await.as_ref()
            && at.elapsed() < TTL
        {
            return cached.clone();
        }
        match list(client, &namespace()).await {
            Ok(workloads) => {
                *self.inner.write().await = Some((Instant::now(), workloads.clone()));
                workloads
            }
            Err(e) => {
                tracing::warn!(error = %e, "listing workloads failed");
                self.inner
                    .read()
                    .await
                    .as_ref()
                    .map(|(_, cached)| cached.clone())
                    .unwrap_or_default()
            }
        }
    }
}

/// Every Deployment and StatefulSet in `namespace`, sorted by name.
pub async fn list(client: &Client, namespace: &str) -> anyhow::Result<Vec<Workload>> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let statefulsets: Api<StatefulSet> = Api::namespaced(client.clone(), namespace);

    let mut out = Vec::new();
    for d in deployments.list(&ListParams::default()).await?.items {
        out.push(Workload {
            name: d.metadata.name.clone().unwrap_or_default(),
            kind: "Deployment".into(),
            image: container_image(d.spec.as_ref().map(|s| &s.template)),
            ready: d
                .status
                .as_ref()
                .and_then(|s| s.ready_replicas)
                .unwrap_or(0),
            desired: d.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0),
        });
    }
    for s in statefulsets.list(&ListParams::default()).await?.items {
        out.push(Workload {
            name: s.metadata.name.clone().unwrap_or_default(),
            kind: "StatefulSet".into(),
            image: container_image(s.spec.as_ref().map(|s| &s.template)),
            ready: s
                .status
                .as_ref()
                .and_then(|s| s.ready_replicas)
                .unwrap_or(0),
            desired: s.spec.as_ref().and_then(|s| s.replicas).unwrap_or(0),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The first container's image — the one a reader means by "the image".
fn container_image(template: Option<&PodTemplateSpec>) -> String {
    template
        .and_then(|t| t.spec.as_ref())
        .and_then(|s| s.containers.first())
        .and_then(|c| c.image.clone())
        .unwrap_or_default()
}

/// The workloads that make up an app: those whose names start with any of
/// `prefixes`, in the snapshot's order (sorted by name).
pub fn workloads_for(workloads: &[Workload], prefixes: &[&str]) -> Vec<Workload> {
    workloads
        .iter()
        .filter(|w| prefixes.iter().any(|p| w.name.starts_with(p)))
        .cloned()
        .collect()
}

/// Distinct images of the workloads whose names start with any of `prefixes`.
/// Container suffixes (`-coordinator`, `-worker`, `-scheduler`) collapse into one
/// entry per distinct image, which is what a human reads.
pub fn images_for(workloads: &[Workload], prefixes: &[&str]) -> Vec<String> {
    let mut images: Vec<String> = Vec::new();
    for w in workloads_for(workloads, prefixes) {
        if w.image.is_empty() {
            continue;
        }
        if !images.contains(&w.image) {
            images.push(w.image.clone());
        }
    }
    images
}

/// Replicas across `workloads` as `(ready, desired)`. Scale-to-zero workloads
/// (a JupyterHub `user-placeholder`, say) report `0/0`, which is a fact, not a
/// failure — the caller decides how to read it.
pub fn replicas(workloads: &[Workload]) -> (i32, i32) {
    workloads.iter().fold((0, 0), |(ready, desired), w| {
        (ready + w.ready, desired + w.desired)
    })
}

/// Plausible in-memory workloads so `make demo` shows the same UI without a cluster.
fn demo_workloads() -> Vec<Workload> {
    let specs = [
        ("hub", "Deployment", "quay.io/jupyterhub/k8s-hub:4.4.3", 1),
        (
            "lakekeeper",
            "Deployment",
            "quay.io/lakekeeper/catalog:v0.12.2",
            1,
        ),
        ("nova", "Deployment", "ghcr.io/stardelt/nova:dev", 1),
        (
            "proxy",
            "Deployment",
            "quay.io/jupyterhub/configurable-http-proxy:5.2.0",
            1,
        ),
        ("seaweedfs-s3", "Deployment", "chrislusf/seaweedfs:4.25", 1),
        (
            "stardelt-operator",
            "Deployment",
            "ghcr.io/stardelt/stardelt-operator:dev",
            1,
        ),
        ("trino-coordinator", "Deployment", "trinodb/trino:480", 1),
        ("trino-worker", "Deployment", "trinodb/trino:480", 1),
        (
            "user-placeholder",
            "StatefulSet",
            "registry.k8s.io/pause:3.10.1",
            0,
        ),
        (
            "user-scheduler",
            "Deployment",
            "registry.k8s.io/kube-scheduler:v1.30.14",
            2,
        ),
    ];
    let mut out: Vec<Workload> = specs
        .iter()
        .map(|(name, kind, image, replicas)| Workload {
            name: (*name).into(),
            kind: (*kind).into(),
            image: (*image).into(),
            ready: *replicas,
            desired: *replicas,
        })
        .collect();
    // The live snapshot is sorted by name; the demo one should be too.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `GET /api/workloads` — every workload Nova can see, with the image it runs.
pub async fn list_handler(State(s): State<Arc<AppState>>) -> Json<Vec<Workload>> {
    Json(s.workloads.snapshot(s.kube.as_ref(), s.demo).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workload(name: &str, image: &str) -> Workload {
        Workload {
            name: name.into(),
            kind: "Deployment".into(),
            image: image.into(),
            ready: 1,
            desired: 1,
        }
    }

    #[test]
    fn workload_names_collapse_to_distinct_images() {
        let workloads = vec![
            workload("trino-coordinator", "trinodb/trino:480"),
            workload("trino-worker", "trinodb/trino:480"),
            workload("nova", "ghcr.io/stardelt/nova:dev"),
        ];
        assert_eq!(
            images_for(&workloads, &["trino-"]),
            vec!["trinodb/trino:480"]
        );
        assert_eq!(
            images_for(&workloads, &["nova"]),
            vec!["ghcr.io/stardelt/nova:dev"]
        );
        assert!(images_for(&workloads, &["superset"]).is_empty());
    }

    #[test]
    fn digest_pins_are_reported_verbatim() {
        let pinned = "ghcr.io/stardelt/nova@sha256:7a70a64e2b69d9d9c4738c33ca6fdb322e3072fe6791a70d04d20ad68aae41f5";
        let workloads = vec![workload("nova", pinned)];
        assert_eq!(images_for(&workloads, &["nova"]), vec![pinned]);
    }

    #[test]
    fn an_apps_workloads_are_the_ones_sharing_its_name() {
        let workloads = vec![
            workload("hub", "quay.io/jupyterhub/k8s-hub:4.4.3"),
            workload("proxy", "quay.io/jupyterhub/configurable-http-proxy:5.2.0"),
            workload("trino-coordinator", "trinodb/trino:480"),
        ];
        let hub = workloads_for(&workloads, &["hub", "proxy"]);
        let names: Vec<&str> = hub.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["hub", "proxy"]);
        assert!(workloads_for(&workloads, &["superset"]).is_empty());
    }

    #[test]
    fn replicas_add_up_across_an_apps_workloads() {
        let mut frontend = workload("proxy", "quay.io/jupyterhub/configurable-http-proxy:5.2.0");
        frontend.ready = 3;
        frontend.desired = 4;
        let mut placeholder = workload("user-placeholder", "");
        placeholder.kind = "StatefulSet".into();
        placeholder.ready = 0;
        placeholder.desired = 0;

        assert_eq!(replicas(&[frontend, placeholder]), (3, 4));
        assert_eq!(replicas(&[]), (0, 0));
    }

    #[test]
    fn empty_images_are_skipped() {
        let workloads = vec![
            workload("nova", ""),
            workload("kube-scheduler", "registry.k8s.io/x:1"),
        ];
        assert!(images_for(&workloads, &["nova"]).is_empty());
        assert_eq!(
            images_for(&workloads, &["kube-"]),
            vec!["registry.k8s.io/x:1"]
        );
    }
}
