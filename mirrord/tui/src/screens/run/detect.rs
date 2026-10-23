//! Works out what to run without asking: which project this folder is, and which cluster target
//! it most likely corresponds to.
//!
//! Every guess carries the reason it was made (`matches the folder name`, `you ran it last
//! time`), so the review screen can show why, and the user can tell a good guess from a lucky one.

use std::path::Path;

use crate::screens::targets::browser::{TargetItem, TargetKind};

/// Containers that are never the user's app: service mesh proxies and mirrord's own agent.
const SIDECARS: &[&str] = &[
    "istio-proxy",
    "linkerd-proxy",
    "envoy",
    "mirrord-agent",
    "cloud-sql-proxy",
];

/// Where a name taken from the folder itself, rather than a manifest, comes from.
pub const FOLDER_NAME_SOURCE: &str = "the folder name";

/// A name the project in the folder goes by, and where it was found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectName {
    pub name: String,
    pub source: &'static str,
}

/// The names the project in `dir` goes by, most telling first: what its manifest calls it, then
/// the folder it is in. Duplicates are dropped.
pub fn project_names(dir: &Path) -> Vec<ProjectName> {
    let mut names: Vec<ProjectName> = Vec::new();
    let mut push = |name: Option<String>, source: &'static str| {
        let Some(name) = name.map(|name| name.trim().to_owned()) else {
            return;
        };
        if !name.is_empty() && !names.iter().any(|existing| existing.name == name) {
            names.push(ProjectName { name, source });
        }
    };

    let read = |file: &str| std::fs::read_to_string(dir.join(file)).ok();

    push(
        read("package.json")
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|json| json.get("name")?.as_str().map(str::to_owned))
            // `@team/checkout` deploys as `checkout`.
            .map(|name| name.rsplit('/').next().unwrap_or(&name).to_owned()),
        "package.json",
    );
    push(
        read("Cargo.toml")
            .and_then(|text| text.parse::<toml::Table>().ok())
            .and_then(|table| {
                table
                    .get("package")?
                    .get("name")?
                    .as_str()
                    .map(str::to_owned)
            }),
        "Cargo.toml",
    );
    push(
        read("pyproject.toml")
            .and_then(|text| text.parse::<toml::Table>().ok())
            .and_then(|table| {
                table
                    .get("project")?
                    .get("name")?
                    .as_str()
                    .map(str::to_owned)
            }),
        "pyproject.toml",
    );
    push(
        read("go.mod").and_then(|text| {
            let module = text.lines().find_map(|line| line.strip_prefix("module "))?;
            module.trim().rsplit('/').next().map(str::to_owned)
        }),
        "go.mod",
    );
    push(
        dir.file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        FOLDER_NAME_SOURCE,
    );

    names
}

/// The lowest score of a sure guess: an exact name match. A close match plus the best kind
/// weight stays under it.
const SURE_SCORE: u32 = 60;

/// The port the project's app listens on when run locally, and the file that says so: a `PORT`
/// in an `.env` file, a port in the package.json scripts (`PORT=3000`, `--port 3000`, `-p 3000`),
/// or the Dockerfile's `EXPOSE`, first found wins. Used to map the local port to the target's
/// when they differ, so the user never has to notice the mismatch.
pub fn local_port(dir: &Path) -> Option<(u16, &'static str)> {
    let read = |file: &str| std::fs::read_to_string(dir.join(file)).ok();

    for file in [".env.local", ".env.development", ".env"] {
        let port = read(file).and_then(|text| {
            text.lines().find_map(|line| {
                let value = line.trim().strip_prefix("PORT=")?;
                value.trim().trim_matches(['"', '\'']).parse().ok()
            })
        });
        if let Some(port) = port {
            return Some((port, ".env"));
        }
    }

    let scripts = read("package.json")
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|json| json.get("scripts").cloned());
    for script in ["dev", "start", "serve"] {
        let Some(line) = scripts
            .as_ref()
            .and_then(|scripts| scripts.get(script))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        if let Some(port) = port_in_command(line) {
            return Some((port, "package.json"));
        }
    }

    read("Dockerfile").and_then(|text| {
        text.lines().find_map(|line| {
            let rest = line.trim().strip_prefix("EXPOSE ")?;
            let first = rest.split_whitespace().next()?;
            first
                .split('/')
                .next()?
                .parse()
                .ok()
                .map(|port| (port, "Dockerfile"))
        })
    })
}

/// A port a command line sets: `PORT=3000`, `--port 3000`, `--port=3000` or `-p 3000`.
fn port_in_command(line: &str) -> Option<u16> {
    let words: Vec<&str> = line.split_whitespace().collect();
    words.iter().enumerate().find_map(|(index, word)| {
        let after = || words.get(index + 1).and_then(|next| next.parse().ok());
        match *word {
            "--port" | "-p" => after(),
            _ => word
                .strip_prefix("PORT=")
                .or_else(|| word.strip_prefix("--port="))
                .and_then(|value| value.parse().ok()),
        }
    })
}

/// A cluster target the project could run against, and why it is offered.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub namespace: String,
    pub kind: TargetKind,
    pub name: String,
    /// The container to target, when the workload has more than one worth choosing between.
    pub container: Option<String>,
    pub score: u32,
    pub reason: String,
}

impl Candidate {
    /// How the target is listed for picking: its path and namespace, such as
    /// `deployment/api (shop)`.
    pub fn label(&self) -> String {
        format!("{} ({})", self.path(), self.namespace)
    }

    /// Whether the guess is one to trust without looking: the user ran it from here before, or
    /// it is named exactly like the project. Looser matches are worth a glance.
    pub fn is_sure(&self) -> bool {
        self.score >= SURE_SCORE
    }

    /// The mirrord target path, such as `deployment/api/container/app`.
    pub fn path(&self) -> String {
        match &self.container {
            Some(container) => format!("{}/{}/container/{container}", self.kind, self.name),
            None => format!("{}/{}", self.kind, self.name),
        }
    }
}

/// Ranks every target against the project's names, best first.
///
/// What scores, highest first:
/// - the user ran this target from here before (`ran_before`)
/// - its name is one of the project's names
/// - its name contains a project name or the other way round (`checkout` and `checkout-api`)
/// - it shares a word with a project name (`orders-service` and `orders-worker`)
///
/// Kinds people usually develop against (deployments, rollouts, statefulsets) win ties over pods
/// and jobs. Targets that match nothing are still listed, after the rest, so a guess never hides
/// the one the user wanted.
pub fn rank(
    listings: &[(String, TargetKind, Vec<TargetItem>)],
    names: &[ProjectName],
    ran_before: impl Fn(&str, &str) -> bool,
) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = listings
        .iter()
        .flat_map(|(namespace, kind, items)| items.iter().map(move |item| (namespace, *kind, item)))
        .map(|(namespace, kind, item)| {
            let (container, container_reason) = pick_container(&item.containers, names);
            let path = match &container {
                Some(container) => format!("{kind}/{}/container/{container}", item.name),
                None => format!("{kind}/{}", item.name),
            };

            let (mut score, mut reason) = match_score(&item.name, names);
            if ran_before(namespace, &path) {
                score += 100;
                reason = "ran from here before".to_owned();
            }
            score += kind_weight(kind);
            if let Some(container_reason) = container_reason {
                reason = format!("{reason}; {container_reason}");
            }

            Candidate {
                namespace: namespace.clone(),
                kind,
                name: item.name.clone(),
                container,
                score,
                reason,
            }
        })
        .collect();

    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.name.cmp(&right.name))
    });
    candidates
}

/// How well `target` matches the project's names, and the reason to show for it.
fn match_score(target: &str, names: &[ProjectName]) -> (u32, String) {
    let target = target.to_lowercase();
    let mut best = (0, "no name match".to_owned());

    for project in names {
        let name = project.name.to_lowercase();
        let (score, how) = if target == name {
            (60, "named like")
        } else if target.contains(&name) || name.contains(&target) {
            (40, "close to")
        } else if words(&target)
            .any(|word| word.len() > 2 && words(&name).any(|other| other == word))
        {
            (20, "shares a word with")
        } else {
            continue;
        };
        if score > best.0 {
            best = (
                score,
                format!("{how} {} ({})", project.name, project.source),
            );
        }
    }
    best
}

fn words(name: &str) -> impl Iterator<Item = &str> {
    name.split(['-', '_', '.', '/'])
        .filter(|word| !word.is_empty())
}

fn kind_weight(kind: TargetKind) -> u32 {
    match kind {
        TargetKind::Deployment | TargetKind::Rollout => 5,
        TargetKind::StatefulSet => 4,
        TargetKind::Service => 3,
        TargetKind::Pod | TargetKind::ReplicaSet => 1,
        TargetKind::Job | TargetKind::CronJob => 0,
    }
}

/// The container to target in a workload, and why, when there is a choice to make. Sidecars are
/// never picked; of the rest, one named like the project wins, else the first.
fn pick_container(
    containers: &[String],
    names: &[ProjectName],
) -> (Option<String>, Option<String>) {
    let apps: Vec<&String> = containers
        .iter()
        .filter(|container| !SIDECARS.contains(&container.as_str()))
        .collect();
    if containers.len() <= 1 {
        return (None, None);
    }

    let named = apps.iter().find(|container| {
        names
            .iter()
            .any(|project| container.eq_ignore_ascii_case(&project.name))
    });
    match (named, apps.first()) {
        (Some(container), _) => (
            Some((*container).clone()),
            Some(format!("container {container}, named like the project")),
        ),
        (None, Some(container)) if apps.len() == 1 => (
            Some((*container).clone()),
            Some(format!("container {container}, sidecars skipped")),
        ),
        (None, Some(container)) => (
            Some((*container).clone()),
            Some(format!("container {container}, first of {}", apps.len())),
        ),
        (None, None) => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ProjectName, local_port, port_in_command, project_names, rank};

    #[test]
    fn the_local_port_comes_from_env_then_scripts_then_the_dockerfile() {
        let dir = std::env::temp_dir().join(format!("mirrord-tui-port-{}", std::process::id()));
        _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Dockerfile"), "FROM node\nEXPOSE 8080/tcp\n").unwrap();
        assert_eq!(local_port(&dir), Some((8080, "Dockerfile")));
        std::fs::write(
            dir.join("package.json"),
            r#"{ "scripts": { "dev": "next dev -p 3001" } }"#,
        )
        .unwrap();
        assert_eq!(local_port(&dir), Some((3001, "package.json")));
        std::fs::write(dir.join(".env"), "NODE_ENV=dev\nPORT=\"4000\"\n").unwrap();
        assert_eq!(local_port(&dir), Some((4000, ".env")));
        _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ports_in_command_lines() {
        assert_eq!(port_in_command("PORT=3000 node server.js"), Some(3000));
        assert_eq!(port_in_command("vite --port=5173"), Some(5173));
        assert_eq!(port_in_command("uvicorn app:main --port 8000"), Some(8000));
        assert_eq!(port_in_command("node server.js"), None);
    }
    use crate::screens::targets::browser::{TargetItem, TargetKind};

    fn project(dir: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("mirrord-tui-detect-{}", std::process::id()));
        let dir = root.join(dir);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        dir
    }

    fn item(name: &str, containers: &[&str]) -> TargetItem {
        TargetItem {
            name: name.to_owned(),
            containers: containers.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    #[test]
    fn manifests_name_the_project_before_the_folder() {
        let dir = project(
            "web-folder",
            &[
                ("package.json", r#"{ "name": "@shop/checkout" }"#),
                ("go.mod", "module github.com/shop/orders\n"),
            ],
        );
        let names: Vec<String> = project_names(&dir)
            .into_iter()
            .map(|name| name.name)
            .collect();
        assert_eq!(names, ["checkout", "orders", "web-folder"]);
    }

    #[test]
    fn ranks_name_matches_first_and_says_why() {
        let names = vec![ProjectName {
            name: "checkout".to_owned(),
            source: "package.json",
        }];
        let listings = vec![
            (
                "shop".to_owned(),
                TargetKind::Deployment,
                vec![
                    item("payments", &[]),
                    item("checkout-api", &["app", "istio-proxy"]),
                ],
            ),
            (
                "shop".to_owned(),
                TargetKind::Pod,
                vec![item("checkout", &[])],
            ),
        ];

        let ranked = rank(&listings, &names, |_, _| false);
        assert_eq!(ranked[0].name, "checkout");
        assert!(
            ranked[0].reason.contains("named like checkout"),
            "{}",
            ranked[0].reason
        );

        let api = ranked
            .iter()
            .find(|candidate| candidate.name == "checkout-api")
            .unwrap();
        assert_eq!(api.container.as_deref(), Some("app"));
        assert_eq!(api.path(), "deployment/checkout-api/container/app");
        assert!(api.reason.contains("sidecars skipped"), "{}", api.reason);

        // Nothing matches payments, but it is still offered, last.
        assert_eq!(
            ranked.last().map(|candidate| candidate.name.as_str()),
            Some("payments")
        );
    }

    #[test]
    fn a_target_run_before_wins() {
        let names = vec![ProjectName {
            name: "checkout".to_owned(),
            source: "the folder name",
        }];
        let listings = vec![(
            "shop".to_owned(),
            TargetKind::Deployment,
            vec![item("checkout", &[]), item("legacy", &[])],
        )];
        let ranked = rank(&listings, &names, |namespace, path| {
            namespace == "shop" && path == "deployment/legacy"
        });
        assert_eq!(ranked[0].name, "legacy");
        assert_eq!(ranked[0].reason, "ran from here before");
    }
}
