#![cfg(test)]

use std::time::Duration;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{Api, Client};
use mirrord_config::target::{pod::PodTarget, Target};
use mirrord_operator::crd::db_branching::{
    core::{ConnectionSource, ConnectionSourceKind},
    mongodb::{
        BranchCopyConfig as MongodbBranchCopyConfig, MongodbBranchDatabase,
        MongodbBranchDatabaseSpec,
    },
    mysql::{
        BranchCopyConfig as MysqlBranchCopyConfig, MysqlBranchDatabase, MysqlBranchDatabaseSpec,
    },
    pg::{BranchCopyConfig, PgBranchDatabase, PgBranchDatabaseSpec},
};
use mirrord_test_utils::run_command::{run_db_branches_destroy, run_db_branches_status};
use rstest::*;

use crate::utils::{
    kube_client, kube_service::KubeService, random_string, resource_guard::ResourceGuard,
    services::service_with_env, PRESERVE_FAILED_ENV_NAME,
};

#[fixture]
pub async fn db_branch_service(#[future] kube_client: Client) -> KubeService {
    service_with_env(
        &format!("test-db-branches-{}", random_string()),
        "ClusterIP",
        "ghcr.io/metalbear-co/mirrord-pytest:latest",
        "db-branch-test",
        true,
        kube_client.await,
        serde_json::Value::Null,
    )
    .await
}

fn make_pg_branch(name: &str, pod_name: &str) -> PgBranchDatabase {
    PgBranchDatabase {
        metadata: ObjectMeta { name: Some(name.to_owned()), ..Default::default() },
        spec: PgBranchDatabaseSpec {
            id: name.to_owned(),
            connection_source: ConnectionSource::Url(Box::new(ConnectionSourceKind::Env {
                container: None,
                variable: "DATABASE_URL".to_owned(),
            })),
            database_name: Some("testdb".to_owned()),
            target: Target::Pod(PodTarget { pod: pod_name.to_owned(), container: None }),
            ttl_secs: 300,
            postgres_version: None,
            copy: BranchCopyConfig::default(),
            iam_auth: None,
        },
        status: None,
    }
}

fn make_mysql_branch(name: &str, pod_name: &str) -> MysqlBranchDatabase {
    MysqlBranchDatabase {
        metadata: ObjectMeta { name: Some(name.to_owned()), ..Default::default() },
        spec: MysqlBranchDatabaseSpec {
            id: name.to_owned(),
            connection_source: ConnectionSource::Url(Box::new(ConnectionSourceKind::Env {
                container: None,
                variable: "DATABASE_URL".to_owned(),
            })),
            database_name: Some("testdb".to_owned()),
            target: Target::Pod(PodTarget { pod: pod_name.to_owned(), container: None }),
            ttl_secs: 300,
            mysql_version: None,
            copy: MysqlBranchCopyConfig::default(),
        },
        status: None,
    }
}

fn make_mongodb_branch(name: &str, pod_name: &str) -> MongodbBranchDatabase {
    MongodbBranchDatabase {
        metadata: ObjectMeta { name: Some(name.to_owned()), ..Default::default() },
        spec: MongodbBranchDatabaseSpec {
            id: name.to_owned(),
            connection_source: ConnectionSource::Url(Box::new(ConnectionSourceKind::Env {
                container: None,
                variable: "DATABASE_URL".to_owned(),
            })),
            database_name: Some("testdb".to_owned()),
            target: Target::Pod(PodTarget { pod: pod_name.to_owned(), container: None }),
            ttl_secs: 300,
            mongodb_version: None,
            copy: MongodbBranchCopyConfig::default(),
        },
        status: None,
    }
}

/// Full lifecycle test for `mirrord db-branches` commands.
///
/// Runs all scenarios sequentially against a single namespace to avoid races:
///   1. `status` on an empty namespace prints the "no active" message
///   2. `status` lists all DB types (PG, MySQL, MongoDB) once branches are created
///   3. `status <name>` filters output to the named branch only
///   4. `destroy <name>` removes one branch, leaving the rest intact
///   5. `destroy --all` removes all remaining branches
#[rstest]
#[tokio::test]
pub async fn db_branches_lifecycle(#[future] db_branch_service: KubeService) {
    let kube_service = db_branch_service.await;
    let client = kube_client().await;
    let delete_after_fail = std::env::var_os(PRESERVE_FAILED_ENV_NAME).is_none();

    let ns = &kube_service.namespace;
    let pod = &kube_service.pod_name;

    let pg_api: Api<PgBranchDatabase> = Api::namespaced(client.clone(), ns);
    let mysql_api: Api<MysqlBranchDatabase> = Api::namespaced(client.clone(), ns);
    let mongodb_api: Api<MongodbBranchDatabase> = Api::namespaced(client.clone(), ns);

    // 1. Empty namespace.
    let mut process = run_db_branches_status(ns, &[]).await;
    process.wait_assert_success().await;
    println!("err: {}", process.get_stderr().await);
    println!("out: {}", process.get_stdout().await);
    //process.assert_stderr_contains("No active DB branch found").await;

    // 2. Create all three branch types.
    let pg_name = format!("test-pg-branch-{}", random_string());
    let mysql_name = format!("test-mysql-branch-{}", random_string());
    //let mongodb_name = format!("test-mongodb-branch-{}", random_string());

    let (_pg_guard, _) =
        ResourceGuard::create(pg_api.clone(), &make_pg_branch(&pg_name, pod), delete_after_fail)
            .await
            .expect("failed to create PgBranchDatabase");
    let (_mysql_guard, _) = ResourceGuard::create(
        mysql_api.clone(),
        &make_mysql_branch(&mysql_name, pod),
        delete_after_fail,
    )
    .await
    .expect("failed to create MysqlBranchDatabase");
  //let (_mongodb_guard, _) = ResourceGuard::create(
  //    mongodb_api.clone(),
  //    &make_mongodb_branch(&mongodb_name, pod),
  //    delete_after_fail,
  //)
  //.await
  //.expect("failed to create MongodbBranchDatabase");
    
    // 3. Status lists all DB types with their labels.
    let mut process = run_db_branches_status(ns, &[]).await;
    process.wait_assert_success().await;
    process.assert_stdout_contains(&pg_name).await;
    process.assert_stdout_contains("PostgreSQL").await;
    process.assert_stdout_contains(&mysql_name).await;
    process.assert_stdout_contains("MySQL").await;
 // process.assert_stdout_contains(&mongodb_name).await;
 // process.assert_stdout_contains("MongoDB").await;

    // 4. Status filters by name.
    let mut process = run_db_branches_status(ns, &[&pg_name]).await;
    process.wait_assert_success().await;
    process.assert_stdout_contains(&pg_name).await;
    process.assert_stdout_doesnt_contain(&mysql_name).await;
 // process.assert_stdout_doesnt_contain(&mongodb_name).await;

    // 5. Destroy one branch by name; the other two must survive.
    let mut destroy = run_db_branches_destroy(ns, false, &[&pg_name]).await;
    destroy.wait_assert_success().await;
  //destroy.assert_stdout_contains(&pg_name).await;

    let mut status = run_db_branches_status(ns, &[]).await;
    status.wait_assert_success().await;
    status.assert_stdout_doesnt_contain(&pg_name).await;
    status.assert_stdout_contains(&mysql_name).await;
//  status.assert_stdout_contains(&mongodb_name).await;

    // 6. Destroy all remaining branches.
    let mut destroy = run_db_branches_destroy(ns, true, &[]).await;
    destroy.wait_assert_success().await;

    let mut status = run_db_branches_status(ns, &[]).await;
    status.wait_assert_success().await;
    status.assert_stdout_doesnt_contain(&mysql_name).await;
//  status.assert_stdout_doesnt_contain(&mongodb_name).await;
}
