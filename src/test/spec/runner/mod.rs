mod operation;
mod test_event;
mod test_file;

use std::time::Duration;

use crate::{
    bson::doc,
    concern::{Acknowledgment, WriteConcern},
    operation::RunCommand,
    options::{ClientOptions, CollectionOptions},
    test::{assert_matches, util::EventClient, TestClient, CLIENT_OPTIONS},
};

pub use self::{
    operation::AnyTestOperation,
    test_event::TestEvent,
    test_file::{OperationObject, TestCase, TestData, TestFile},
};

const SKIPPED_OPERATIONS: &[&str] = &[
    "bulkWrite",
    "count",
    "download",
    "download_by_name",
    "listCollectionObjects",
    "listDatabaseObjects",
    "listIndexNames",
    "listIndexes",
    "mapReduce",
    "watch",
];

pub async fn run_v2_test(test_file: TestFile) {
    for test_case in test_file.tests {
        let has_skipped_op = test_case
            .operations
            .iter()
            .any(|op| SKIPPED_OPERATIONS.contains(&op.name.as_str()));
        if has_skipped_op {
            continue;
        }

        if let Some(skip_reason) = test_case.skip_reason {
            println!("Skipping {}: {}", test_case.description, skip_reason);
            continue;
        }

        let client = create_client(test_case.client_options, test_case.use_multiple_mongoses).await;

        if let Some(ref run_on) = test_file.run_on {
            let can_run_on = run_on.iter().any(|run_on| run_on.can_run_on(&client));
            if !can_run_on {
                println!("Skipping {}", test_case.description);
                continue;
            }
        }

        let db_name = match test_file.database_name {
            Some(ref db_name) => db_name.clone(),
            None => test_case.description.replace('$', "%").replace(' ', "_"),
        };

        let coll_name = match test_file.collection_name {
            Some(ref coll_name) => coll_name.clone(),
            None => "coll".to_string(),
        };

        if test_case
            .description
            .contains("Aggregate with $listLocalSessions")
        {
            // TODO DRIVERS-1230: This test currently fails on 3.6 standalones because the session
            // does not attach to the server ping. When the driver is updated to send implicit
            // sessions to standalones, this test should be unskipped.
            let req = semver::VersionReq::parse("<= 3.6").unwrap();
            if req.matches(&client.server_version) && client.is_standalone() {
                continue;
            }
            start_session(&client, &db_name).await;
        }

        if let Some(ref data) = test_file.data {
            match data {
                TestData::Single(data) => {
                    if !data.is_empty() {
                        let coll = if client.is_replica_set() || client.is_sharded() {
                            let write_concern =
                                WriteConcern::builder().w(Acknowledgment::Majority).build();
                            let options = CollectionOptions::builder()
                                .write_concern(write_concern)
                                .build();
                            client
                                .init_db_and_coll_with_options(&db_name, &coll_name, options)
                                .await
                        } else {
                            client.init_db_and_coll(&db_name, &coll_name).await
                        };
                        coll.insert_many(data.clone(), None)
                            .await
                            .expect(&test_case.description);
                    }
                }
                TestData::Many(_) => panic!("{}: invalid data format", &test_case.description),
            }
        }

        if let Some(ref fail_point) = test_case.fail_point {
            client
                .database("admin")
                .run_command(fail_point.clone(), None)
                .await
                .unwrap();
        }

        let mut events: Vec<TestEvent> = Vec::new();
        for operation in test_case.operations {
            let result = match operation.object {
                Some(OperationObject::Client) => client.run_client_operation(&operation).await,
                Some(OperationObject::Database) => {
                    client.run_database_operation(&operation, &db_name).await
                }
                Some(OperationObject::Collection) | None => {
                    client
                        .run_collection_operation(
                            &operation,
                            &db_name,
                            &coll_name,
                            operation.collection_options.clone(),
                        )
                        .await
                }
                Some(OperationObject::GridfsBucket) => {
                    panic!("unsupported operation: {}", operation.name)
                }
            };
            let mut operation_events: Vec<TestEvent> = client
                .collect_events(&operation)
                .into_iter()
                .map(Into::into)
                .collect();

            if let Some(error) = operation.error {
                assert_eq!(
                    result.is_err(),
                    error,
                    "{}: expected error: {}, got {:?}",
                    test_case.description,
                    error,
                    result
                );
            }

            if let Some(expected_result) = operation.result {
                let description = &test_case.description;
                let result = result
                    .unwrap()
                    .unwrap_or_else(|| panic!("{:?}: operation should succeed", description));
                assert_matches(&result, &expected_result, Some(description));
            }

            events.append(&mut operation_events);
        }

        if let Some(expectations) = test_case.expectations {
            assert_eq!(events.len(), expectations.len());
            for (actual_event, expected_event) in events.iter().zip(expectations.iter()) {
                assert_matches(actual_event, expected_event, None);
            }
        }

        if let Some(outcome) = test_case.outcome {
            assert!(outcome.matches_actual(db_name, coll_name, &client).await);
        }

        if test_case.fail_point.is_some() {
            client
                .database("admin")
                .run_command(
                    doc! {
                        "configureFailPoint": "failCommand",
                        "mode": "off"
                    },
                    None,
                )
                .await
                .unwrap();
        }
    }

    async fn create_client(
        options: Option<ClientOptions>,
        use_multiple_mongoses: Option<bool>,
    ) -> EventClient {
        let mut options = match options {
            Some(mut options) => {
                options.hosts = CLIENT_OPTIONS.hosts.clone();
                merge_options!(
                    CLIENT_OPTIONS.clone(),
                    &mut options,
                    [
                        app_name,
                        compressors,
                        cmap_event_handler,
                        command_event_handler,
                        connect_timeout,
                        credential,
                        direct_connection,
                        driver_info,
                        heartbeat_freq,
                        local_threshold,
                        max_idle_time,
                        max_pool_size,
                        min_pool_size,
                        read_concern,
                        repl_set_name,
                        retry_reads,
                        retry_writes,
                        selection_criteria,
                        server_selection_timeout,
                        socket_timeout,
                        tls,
                        wait_queue_timeout,
                        write_concern,
                        zlib_compression,
                        original_srv_hostname,
                        original_uri
                    ]
                );
                options
            }
            None => CLIENT_OPTIONS.clone(),
        };
        if TestClient::new().await.is_sharded() && use_multiple_mongoses != Some(true) {
            options.hosts = options.hosts.iter().cloned().take(1).collect();
        }
        EventClient::with_options(options).await
    }

    async fn start_session(client: &EventClient, db_name: &str) {
        let mut session = client
            .start_implicit_session_with_timeout(Duration::from_secs(60 * 60))
            .await;
        let op = RunCommand::new(db_name.to_string(), doc! { "ping": 1 }, None).unwrap();
        client
            .execute_operation_with_session(op, &mut session)
            .await
            .unwrap();
    }
}
