//! The last step of the Run view: how mirrord should run the picked folders, as a short list of
//! ready-made configs to choose from instead of a form.
//!
//! Some are made on the spot from what was picked: the three jobs (only my requests, take over,
//! no traffic), and, when the targets use databases, the same with those databases branched. The
//! rest are the config files already in the folders' `.mirrord` directories. Each carries the
//! words a search should find it by (`db`, `postgres`, `kafka`, `steal`, its variable and queue
//! names, every key and value of a file), so typing what you want narrows the list to the
//! configs that do it.

use std::path::PathBuf;

use serde_json::Value;

use crate::screens::{
    config::document,
    run::{Job, cluster::TargetResources},
    targets::suggest::compress_home,
};

/// What picking a config does to the plan.
#[derive(Clone, Debug)]
pub enum Setup {
    /// A config made on the spot.
    Made {
        job: Job,
        /// Branch every database the targets use (their team presets, when there are any).
        branch_databases: bool,
    },
    /// A file from a folder's `.mirrord` directory. Picking it loads its options into the config
    /// step's option tree, where they apply to every folder.
    File { path: PathBuf },
}

#[derive(Clone, Debug)]
pub struct ConfigOption {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub keywords: String,
    pub setup: Setup,
}

/// Every config on offer for `dirs`, the recommended one first. `resources` is what was found on
/// the picked targets.
pub fn options(
    dirs: &[PathBuf],
    has_targets: bool,
    resources: &[TargetResources],
) -> Vec<ConfigOption> {
    let mut options = Vec::new();

    let queues: Vec<String> = resources
        .iter()
        .flat_map(|found| found.queues.iter())
        .map(|queue| format!("{} {}", queue.id, queue.kind))
        .collect();
    let databases: Vec<String> = resources
        .iter()
        .flat_map(|found| {
            let presets = found
                .presets
                .iter()
                .map(|preset| format!("{} \u{b7} {}", preset.engine, preset.id));
            let detected = found
                .databases
                .iter()
                .filter(|database| found.presets.is_empty() && database.engine.branchable())
                .map(|database| format!("{} \u{b7} {}", database.engine.name(), database.label()));
            presets.chain(detected).collect::<Vec<_>>()
        })
        .collect();

    if has_targets {
        let mut detail = "requests with your session key reach you".to_owned();
        if !queues.is_empty() {
            detail.push_str(", queue messages too");
        }
        options.push(ConfigOption {
            id: "made:mine".to_owned(),
            title: "Only my requests".to_owned(),
            detail,
            keywords: format!(
                "recommended default split filter session key header baggage safe shared mine queue {}",
                queues.join(" ")
            ),
            setup: Setup::Made {
                job: Job::OnlyMine,
                branch_databases: false,
            },
        });
        if !databases.is_empty() {
            options.push(ConfigOption {
                id: "made:mine+db".to_owned(),
                title: format!(
                    "Only my requests + branch {}",
                    plural(databases.len(), "database")
                ),
                detail: format!("your own copy of {}", databases.join(", ")),
                keywords: format!(
                    "db database databases branch branching copy data sql split mine {}",
                    databases.join(" ")
                ),
                setup: Setup::Made {
                    job: Job::OnlyMine,
                    branch_databases: true,
                },
            });
        }
        options.push(ConfigOption {
            id: "made:takeover".to_owned(),
            title: "Take over all traffic".to_owned(),
            detail: "every request reaches you; affects everyone".to_owned(),
            keywords: "replace steal copy takeover take over all traffic scale down".to_owned(),
            setup: Setup::Made {
                job: Job::TakeOver,
                branch_databases: false,
            },
        });
    }
    options.push(ConfigOption {
        id: "made:none".to_owned(),
        title: "No incoming traffic".to_owned(),
        detail: "the cluster's network, files and env only".to_owned(),
        keywords: "targetless outgoing env environment files network none no traffic".to_owned(),
        setup: Setup::Made {
            job: Job::NoTraffic,
            branch_databases: false,
        },
    });

    for dir in dirs {
        for (path, content) in document::config_files(dir) {
            let leaves = leaves(&Value::Object(content.clone()), "");
            let summary: Vec<String> = leaves
                .iter()
                .take(3)
                .map(|(key, value)| format!("{key}: {value}"))
                .collect();
            let mut detail = summary.join(" \u{b7} ");
            if leaves.len() > 3 {
                detail.push_str(&format!(" \u{b7} +{} more", leaves.len() - 3));
            }
            if detail.is_empty() {
                detail = "empty file".to_owned();
            }
            options.push(ConfigOption {
                id: format!("file:{}", path.display()),
                // The folder and the file, `checkout/.mirrord/with-db.json`: the whole path
                // rarely fits, and the folder is what tells two `mirrord.json` apart.
                title: format!(
                    "{}/.mirrord/{}",
                    dir.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ),
                detail,
                keywords: format!("{} ", compress_home(&path.display().to_string()))
                    + &leaves
                        .iter()
                        .map(|(key, value)| format!("{key} {value}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                setup: Setup::File { path: path.clone() },
            });
        }
    }
    options
}

/// Every leaf of `value` as a dotted key and a short value, such as
/// `feature.network.incoming: steal`.
fn leaves(value: &Value, prefix: &str) -> Vec<(String, String)> {
    match value {
        Value::Object(map) => map
            .iter()
            .flat_map(|(key, value)| {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                leaves(value, &path)
            })
            .collect(),
        Value::Array(items) if items.iter().all(|item| !item.is_object()) => vec![(
            prefix.to_owned(),
            items.iter().map(short).collect::<Vec<_>>().join(", "),
        )],
        Value::Array(items) => items
            .iter()
            .enumerate()
            .flat_map(|(index, item)| leaves(item, &format!("{prefix}[{index}]")))
            .collect(),
        other => vec![(prefix.to_owned(), short(other))],
    }
}

fn short(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn leaves_flatten_nested_options_into_searchable_keys() {
        let found = leaves(
            &json!({ "feature": { "network": { "incoming": "steal" } }, "target": { "path": "deployment/api" }, "skip": ["a", "b"] }),
            "",
        );
        assert_eq!(
            found,
            vec![
                ("feature.network.incoming".to_owned(), "steal".to_owned()),
                ("skip".to_owned(), "a, b".to_owned()),
                ("target.path".to_owned(), "deployment/api".to_owned()),
            ]
        );
    }

    #[test]
    fn without_targets_only_the_no_traffic_config_is_made() {
        let made = options(&[], false, &[]);
        assert_eq!(
            made.iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            vec!["made:none"]
        );
    }
}
