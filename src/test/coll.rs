use std::time::Duration;

use futures::stream::StreamExt;
use lazy_static::lazy_static;
use semver::VersionReq;

use crate::{
    bson::{doc, Bson, Document},
    concern::{ReadConcern, ReadConcernLevel},
    error::{ErrorKind, Result, WriteFailure},
    event::command::CommandStartedEvent,
    options::{
        AggregateOptions,
        ChangeStreamOptions,
        CollectionOptions,
        DeleteOptions,
        FindOneAndDeleteOptions,
        FindOneOptions,
        FindOptions,
        Hint,
        InsertManyOptions,
        ReadPreference,
        SelectionCriteria,
        UpdateOptions,
    },
    results::DeleteResult,
    test::{
        util::{drop_collection, CommandEvent, EventClient, TestClient},
        LOCK,
    },
    RUNTIME,
};

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn insert_err_details() {
    let _guard = LOCK.run_exclusively().await;

    let client = TestClient::new().await;
    let coll = client
        .init_db_and_coll(function_name!(), function_name!())
        .await;
    if client.server_version_lt(4, 0) || !client.is_replica_set() {
        return;
    }
    client
        .database("admin")
        .run_command(
            doc! {
                "configureFailPoint": "failCommand",
                "data": {
                "failCommands": ["insert"],
                "writeConcernError": {
                    "code": 100,
                    "codeName": "UnsatisfiableWriteConcern",
                    "errmsg": "Not enough data-bearing nodes",
                    "errInfo": {
                    "writeConcern": {
                        "w": 2,
                        "wtimeout": 0,
                        "provenance": "clientSupplied"
                    }
                    }
                }
                },
                "mode": { "times": 1 }
            },
            None,
        )
        .await
        .unwrap();

    let wc_error_result = coll.insert_one(doc! { "test": 1 }, None).await;
    match *wc_error_result.unwrap_err().kind {
        ErrorKind::WriteError(WriteFailure::WriteConcernError(ref wc_error)) => {
            match &wc_error.details {
                Some(doc) => {
                    let result = doc.get_document("writeConcern");
                    match result {
                        Ok(write_concern_doc) => {
                            assert_eq!(write_concern_doc.contains_key("provenance"), true);
                        }
                        Err(e) => panic!("{:?}", e),
                    }
                }
                None => panic!("expected details field"),
            }
        }
        ref e => panic!("expected write concern error, got {:?}", e),
    }
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn count() {
    let _guard = LOCK.run_concurrently().await;

    let client = TestClient::new().await;
    let coll = client
        .init_db_and_coll(function_name!(), function_name!())
        .await;

    assert_eq!(coll.estimated_document_count(None).await.unwrap(), 0);

    let _ = coll.insert_one(doc! { "x": 1 }, None).await.unwrap();
    assert_eq!(coll.estimated_document_count(None).await.unwrap(), 1);

    let result = coll
        .insert_many((1..4).map(|i| doc! { "x": i }).collect::<Vec<_>>(), None)
        .await
        .unwrap();
    assert_eq!(result.inserted_ids.len(), 3);
    assert_eq!(coll.estimated_document_count(None).await.unwrap(), 4);
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn find() {
    let _guard = LOCK.run_concurrently().await;

    let client = TestClient::new().await;
    let coll = client
        .init_db_and_coll(function_name!(), function_name!())
        .await;

    let result = coll
        .insert_many((0i32..5).map(|i| doc! { "x": i }).collect::<Vec<_>>(), None)
        .await
        .unwrap();
    assert_eq!(result.inserted_ids.len(), 5);

    let mut cursor = coll.find(None, None).await.unwrap().enumerate();

    while let Some((i, result)) = cursor.next().await {
        let doc = result.unwrap();
        if i > 4 {
            panic!("expected 4 result, got {}", i);
        }

        assert_eq!(doc.len(), 2);
        assert!(doc.contains_key("_id"));
        assert_eq!(doc.get("x"), Some(&Bson::Int32(i as i32)));
    }
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn update() {
    let _guard = LOCK.run_concurrently().await;

    let client = TestClient::new().await;
    let coll = client
        .init_db_and_coll(function_name!(), function_name!())
        .await;

    let result = coll
        .insert_many((0i32..5).map(|_| doc! { "x": 3 }).collect::<Vec<_>>(), None)
        .await
        .unwrap();
    assert_eq!(result.inserted_ids.len(), 5);

    let update_one_results = coll
        .update_one(doc! {"x": 3}, doc! {"$set": { "x": 5 }}, None)
        .await
        .unwrap();
    assert_eq!(update_one_results.modified_count, 1);
    assert!(update_one_results.upserted_id.is_none());

    let update_many_results = coll
        .update_many(doc! {"x": 3}, doc! {"$set": { "x": 4}}, None)
        .await
        .unwrap();
    assert_eq!(update_many_results.modified_count, 4);
    assert!(update_many_results.upserted_id.is_none());

    let options = UpdateOptions::builder().upsert(true).build();
    let upsert_results = coll
        .update_one(doc! {"b": 7}, doc! {"$set": { "b": 7 }}, options)
        .await
        .unwrap();
    assert_eq!(upsert_results.modified_count, 0);
    assert!(upsert_results.upserted_id.is_some());
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn delete() {
    let _guard = LOCK.run_concurrently().await;

    let client = TestClient::new().await;
    let coll = client
        .init_db_and_coll(function_name!(), function_name!())
        .await;

    let result = coll
        .insert_many((0i32..5).map(|_| doc! { "x": 3 }).collect::<Vec<_>>(), None)
        .await
        .unwrap();
    assert_eq!(result.inserted_ids.len(), 5);

    let delete_one_result = coll.delete_one(doc! {"x": 3}, None).await.unwrap();
    assert_eq!(delete_one_result.deleted_count, 1);

    assert_eq!(coll.count_documents(doc! {"x": 3}, None).await.unwrap(), 4);
    let delete_many_result = coll.delete_many(doc! {"x": 3}, None).await.unwrap();
    assert_eq!(delete_many_result.deleted_count, 4);
    assert_eq!(coll.count_documents(doc! {"x": 3 }, None).await.unwrap(), 0);
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn aggregate_out() {
    let _guard = LOCK.run_concurrently().await;

    let client = TestClient::new().await;
    let db = client.database(function_name!());
    let coll = db.collection(function_name!());

    drop_collection(&coll).await;

    let result = coll
        .insert_many((0i32..5).map(|n| doc! { "x": n }).collect::<Vec<_>>(), None)
        .await
        .unwrap();
    assert_eq!(result.inserted_ids.len(), 5);

    let out_coll = db.collection(&format!("{}_1", function_name!()));
    let pipeline = vec![
        doc! {
            "$match": {
                "x": { "$gt": 1 },
            }
        },
        doc! {"$out": out_coll.name()},
    ];
    drop_collection(&out_coll).await;

    coll.aggregate(pipeline.clone(), None).await.unwrap();
    assert!(db
        .list_collection_names(None)
        .await
        .unwrap()
        .into_iter()
        .any(|name| name.as_str() == out_coll.name()));
    drop_collection(&out_coll).await;

    // check that even with a batch size of 0, a new collection is created.
    coll.aggregate(pipeline, AggregateOptions::builder().batch_size(0).build())
        .await
        .unwrap();
    assert!(db
        .list_collection_names(None)
        .await
        .unwrap()
        .into_iter()
        .any(|name| name.as_str() == out_coll.name()));
}

fn kill_cursors_sent(client: &EventClient) -> bool {
    client
        .command_events
        .read()
        .unwrap()
        .iter()
        .any(|event| match event {
            CommandEvent::CommandStartedEvent(CommandStartedEvent { command_name, .. }) => {
                command_name == "killCursors"
            }
            _ => false,
        })
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn kill_cursors_on_drop() {
    let _guard = LOCK.run_concurrently().await;

    let client = TestClient::new().await;
    let db = client.database(function_name!());
    let coll = db.collection(function_name!());

    drop_collection(&coll).await;

    coll.insert_many(vec![doc! { "x": 1 }, doc! { "x": 2 }], None)
        .await
        .unwrap();

    let event_client = EventClient::new().await;
    let coll = event_client
        .database(function_name!())
        .collection(function_name!());

    let cursor = coll
        .find(None, FindOptions::builder().batch_size(1).build())
        .await
        .unwrap();

    assert!(!kill_cursors_sent(&event_client));

    std::mem::drop(cursor);

    // The `Drop` implementation for `Cursor' spawns a back tasks that emits certain events. If the
    // task hasn't been scheduled yet, we may not see the event here. To account for this, we wait
    // for a small amount of time before checking.
    RUNTIME.delay_for(Duration::from_millis(250)).await;

    assert!(kill_cursors_sent(&event_client));
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn no_kill_cursors_on_exhausted() {
    let _guard = LOCK.run_concurrently().await;

    let client = TestClient::new().await;
    let db = client.database(function_name!());
    let coll = db.collection(function_name!());

    drop_collection(&coll).await;

    coll.insert_many(vec![doc! { "x": 1 }, doc! { "x": 2 }], None)
        .await
        .unwrap();

    let event_client = EventClient::new().await;
    let coll = event_client
        .database(function_name!())
        .collection(function_name!());

    let cursor = coll
        .find(None, FindOptions::builder().build())
        .await
        .unwrap();

    assert!(!kill_cursors_sent(&event_client));

    std::mem::drop(cursor);

    // wait for any tasks to get spawned from `Cursor`'s `Drop`.
    RUNTIME.delay_for(Duration::from_millis(250)).await;
    assert!(!kill_cursors_sent(&event_client));
}

lazy_static! {
    #[allow(clippy::unreadable_literal)]
    static ref LARGE_DOC: Document = doc! {
        "text": "the quick brown fox jumped over the lazy sheep dog",
        "in_reply_to_status_id": 22213321312i64,
        "retweet_count": Bson::Null,
        "contributors": Bson::Null,
        "created_at": ";lkasdf;lkasdfl;kasdfl;kasdkl;ffasdkl;fsadkl;fsad;lfk",
        "geo": Bson::Null,
        "source": "web",
        "coordinates": Bson::Null,
        "in_reply_to_screen_name": "sdafsdafsdaffsdafasdfasdfasdfasdfsadf",
        "truncated": false,
        "entities": {
            "user_mentions": [
                {
                    "indices": [
                        0,
                        9
                    ],
                    "screen_name": "sdafsdaff",
                    "name": "sadfsdaffsadf",
                    "id": 1
                }
            ],
            "urls": [],
            "hashtags": []
        },
        "retweeted": false,
        "place": Bson::Null,
        "user": {
            "friends_count": 123,
            "profile_sidebar_fill_color": "7a7a7a",
            "location": "sdafffsadfsdaf sdaf asdf asdf sdfsdfsdafsdaf asdfff sadf",
            "verified": false,
            "follow_request_sent": Bson::Null,
            "favourites_count": 0,
            "profile_sidebar_border_color": "a3a3a3",
            "profile_image_url": "sadfkljajsdlkffajlksdfjklasdfjlkasdljkf asdjklffjlksadfjlksadfjlksdafjlksdaf",
            "geo_enabled": false,
            "created_at": "sadffasdffsdaf",
            "description": "sdffasdfasdfasdfasdf",
            "time_zone": "Mountain Time (US & Canada)",
            "url": "sadfsdafsadf fsdaljk asjdklf lkjasdf lksadklfffsjdklafjlksdfljksadfjlk",
            "screen_name": "adsfffasdf sdlk;fjjdsakfasljkddfjklasdflkjasdlkfjldskjafjlksadf",
            "notifications": Bson::Null,
            "profile_background_color": "303030",
            "listed_count": 1,
            "lang": "en"
        }
    };
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn large_insert() {
    if std::env::consts::OS != "linux" {
        return;
    }

    let _guard = LOCK.run_concurrently().await;

    let docs = vec![LARGE_DOC.clone(); 35000];

    let client = TestClient::new().await;
    let coll = client
        .init_db_and_coll(function_name!(), function_name!())
        .await;
    assert_eq!(
        coll.insert_many(docs, None)
            .await
            .unwrap()
            .inserted_ids
            .len(),
        35000
    );
}

/// Returns a vector of documents that cannot be sent in one batch (35000 documents).
/// Includes duplicate _id's across different batches.
fn multibatch_documents_with_duplicate_keys() -> Vec<Document> {
    let large_doc = LARGE_DOC.clone();

    let mut docs: Vec<Document> = Vec::new();
    docs.extend(vec![large_doc.clone(); 7498]);

    docs.push(doc! { "_id": 1 });
    docs.push(doc! { "_id": 1 }); // error in first batch, index 7499

    docs.extend(vec![large_doc.clone(); 14999]);
    docs.push(doc! { "_id": 1 }); // error in second batch, index 22499

    docs.extend(vec![large_doc.clone(); 9999]);
    docs.push(doc! { "_id": 1 }); // error in third batch, index 32499

    docs.extend(vec![large_doc; 2500]);

    assert_eq!(docs.len(), 35000);
    docs
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn large_insert_unordered_with_errors() {
    if std::env::consts::OS != "linux" {
        return;
    }

    let _guard = LOCK.run_concurrently().await;

    let docs = multibatch_documents_with_duplicate_keys();

    let client = TestClient::new().await;
    let coll = client
        .init_db_and_coll(function_name!(), function_name!())
        .await;
    let options = InsertManyOptions::builder().ordered(false).build();

    match coll
        .insert_many(docs, options)
        .await
        .expect_err("should get error")
        .kind
        .as_ref()
    {
        ErrorKind::BulkWriteError(ref failure) => {
            let mut write_errors = failure
                .write_errors
                .clone()
                .expect("should have write errors");
            assert_eq!(write_errors.len(), 3);
            write_errors.sort_by(|lhs, rhs| lhs.index.cmp(&rhs.index));

            assert_eq!(write_errors[0].index, 7499);
            assert_eq!(write_errors[1].index, 22499);
            assert_eq!(write_errors[2].index, 32499);
        }
        e => panic!("expected bulk write error, got {:?} instead", e),
    }
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn large_insert_ordered_with_errors() {
    if std::env::consts::OS != "linux" {
        return;
    }

    let _guard = LOCK.run_concurrently().await;

    let docs = multibatch_documents_with_duplicate_keys();

    let client = TestClient::new().await;
    let coll = client
        .init_db_and_coll(function_name!(), function_name!())
        .await;
    let options = InsertManyOptions::builder().ordered(true).build();

    match coll
        .insert_many(docs, options)
        .await
        .expect_err("should get error")
        .kind
        .as_ref()
    {
        ErrorKind::BulkWriteError(ref failure) => {
            let write_errors = failure
                .write_errors
                .clone()
                .expect("should have write errors");
            assert_eq!(write_errors.len(), 1);
            assert_eq!(write_errors[0].index, 7499);
            assert_eq!(
                coll.count_documents(None, None)
                    .await
                    .expect("count should succeed"),
                7499
            );
        }
        e => panic!("expected bulk write error, got {:?} instead", e),
    }
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn empty_insert() {
    let _guard = LOCK.run_concurrently().await;

    let client = TestClient::new().await;
    let coll = client
        .database(function_name!())
        .collection(function_name!());
    match coll
        .insert_many(Vec::new(), None)
        .await
        .expect_err("should get error")
        .kind
        .as_ref()
    {
        ErrorKind::ArgumentError { .. } => {}
        e => panic!("expected argument error, got {:?}", e),
    };
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
async fn find_allow_disk_use() {
    let find_opts = FindOptions::builder().allow_disk_use(true).build();
    allow_disk_use_test(find_opts, Some(true)).await;
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
async fn find_do_not_allow_disk_use() {
    let find_opts = FindOptions::builder().allow_disk_use(false).build();
    allow_disk_use_test(find_opts, Some(false)).await;
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
async fn find_allow_disk_use_not_specified() {
    let find_opts = FindOptions::builder().build();
    allow_disk_use_test(find_opts, None).await;
}

#[function_name::named]
async fn allow_disk_use_test(options: FindOptions, expected_value: Option<bool>) {
    let _guard = LOCK.run_concurrently().await;

    let event_client = EventClient::new().await;
    if event_client.server_version_lt(4, 3) {
        return;
    }
    let coll = event_client
        .database(function_name!())
        .collection(function_name!());
    coll.find(None, options).await.unwrap();

    let events = event_client.get_command_started_events("find");
    assert_eq!(events.len(), 1);

    let allow_disk_use = events[0].command.get_bool("allowDiskUse").ok();
    assert_eq!(allow_disk_use, expected_value);
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn ns_not_found_suppression() {
    let _guard = LOCK.run_concurrently().await;

    let client = TestClient::new().await;
    let coll = client.get_coll(function_name!(), function_name!());
    coll.drop(None).await.expect("drop should not fail");
    coll.drop(None).await.expect("drop should not fail");
}

async fn delete_hint_test(options: Option<DeleteOptions>, name: &str) {
    let _guard = LOCK.run_concurrently().await;

    let client = EventClient::new().await;
    let coll = client.database(name).collection(name);
    let _: Result<DeleteResult> = coll.delete_many(doc! {}, options.clone()).await;

    let events = client.get_command_started_events("delete");
    assert_eq!(events.len(), 1);

    let event_hint = events[0].command.get("hint").cloned();
    let expected_hint = match options {
        Some(options) => options.hint.map(|hint| hint.to_bson()),
        None => None,
    };
    assert_eq!(event_hint, expected_hint);
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn delete_hint_keys_specified() {
    let options = DeleteOptions::builder().hint(Hint::Keys(doc! {})).build();
    delete_hint_test(Some(options), function_name!()).await;
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn delete_hint_string_specified() {
    let options = DeleteOptions::builder()
        .hint(Hint::Name(String::new()))
        .build();
    delete_hint_test(Some(options), function_name!()).await;
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn delete_hint_not_specified() {
    delete_hint_test(None, function_name!()).await;
}

async fn find_one_and_delete_hint_test(options: Option<FindOneAndDeleteOptions>, name: &str) {
    let _guard = LOCK.run_concurrently().await;
    let client = EventClient::new().await;

    let req = VersionReq::parse(">= 4.2").unwrap();
    if options.is_some() && !req.matches(&client.server_version) {
        return;
    }

    let coll = client.database(name).collection(name);
    let _: Result<Option<Document>> = coll.find_one_and_delete(doc! {}, options.clone()).await;

    let events = client.get_command_started_events("findAndModify");
    assert_eq!(events.len(), 1);

    let event_hint = events[0].command.get("hint").cloned();
    let expected_hint = options.and_then(|options| options.hint.map(|hint| hint.to_bson()));
    assert_eq!(event_hint, expected_hint);
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn find_one_and_delete_hint_keys_specified() {
    let options = FindOneAndDeleteOptions::builder()
        .hint(Hint::Keys(doc! {}))
        .build();
    find_one_and_delete_hint_test(Some(options), function_name!()).await;
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn find_one_and_delete_hint_string_specified() {
    let options = FindOneAndDeleteOptions::builder()
        .hint(Hint::Name(String::new()))
        .build();
    find_one_and_delete_hint_test(Some(options), function_name!()).await;
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn find_one_and_delete_hint_not_specified() {
    find_one_and_delete_hint_test(None, function_name!()).await;
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn find_one_and_delete_hint_server_version() {
    let _guard = LOCK.run_concurrently().await;

    let client = EventClient::new().await;
    let coll = client.database(function_name!()).collection("coll");

    let options = FindOneAndDeleteOptions::builder()
        .hint(Hint::Name(String::new()))
        .build();
    let res = coll.find_one_and_delete(doc! {}, options).await;

    let req1 = VersionReq::parse("< 4.2").unwrap();
    let req2 = VersionReq::parse("4.2.*").unwrap();
    if req1.matches(&client.server_version) {
        let error = res.expect_err("find one and delete should fail");
        assert!(matches!(error.kind.as_ref(), ErrorKind::OperationError { .. }));
    } else if req2.matches(&client.server_version) {
        let error = res.expect_err("find one and delete should fail");
        assert!(matches!(error.kind.as_ref(), ErrorKind::CommandError { .. }));
    } else {
        assert!(res.is_ok());
    }
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn no_read_preference_to_standalone() {
    let _guard = LOCK.run_concurrently().await;

    let client = EventClient::new().await;

    if !client.is_standalone() {
        return;
    }

    let options = FindOneOptions::builder()
        .selection_criteria(SelectionCriteria::ReadPreference(
            ReadPreference::SecondaryPreferred {
                options: Default::default(),
            },
        ))
        .build();

    client
        .database(function_name!())
        .collection(function_name!())
        .find_one(None, options)
        .await
        .unwrap();

    let command_started = client.get_successful_command_execution("find").0;

    assert!(!command_started.command.contains_key("$readPreference"));
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn watch_test_coll() {
    let _guard = LOCK.run_concurrently().await;

    let client = EventClient::new().await;
    if client.is_standalone() {
        return;
    }

    let coll = client
        .init_db_and_coll(function_name!(), function_name!())
        .await;
    client
        .database(function_name!())
        .create_collection(function_name!(), None)
        .await
        .unwrap();

    let pipeline = vec![doc! { "$match": { "x": 3 } }];
    let options = ChangeStreamOptions::builder()
        .selection_criteria(SelectionCriteria::ReadPreference(ReadPreference::Primary))
        .read_concern(ReadConcern::from(ReadConcernLevel::Majority))
        .max_await_time(Duration::from_millis(5))
        .batch_size(10)
        .build();
    coll.watch(pipeline, Some(options)).await.unwrap();
}

#[cfg_attr(feature = "tokio-runtime", tokio::test)]
#[cfg_attr(feature = "async-std-runtime", async_std::test)]
#[function_name::named]
async fn watch_options_test() {
    let _guard = LOCK.run_concurrently().await;

    let client = EventClient::new().await;
    if client.is_standalone() {
        return;
    }

    let coll_options = CollectionOptions::builder()
        .selection_criteria(SelectionCriteria::ReadPreference(ReadPreference::Primary))
        .read_concern(ReadConcern::from(ReadConcernLevel::Available))
        .build();

    let coll = client
        .init_db_and_coll_with_options(function_name!(), function_name!(), coll_options)
        .await;

    client
        .database(function_name!())
        .create_collection(function_name!(), None)
        .await
        .unwrap();

    let pipeline = vec![doc! { "$match": { "x": 3 } }];
    let options = ChangeStreamOptions::builder()
        .read_concern(ReadConcern::from(ReadConcernLevel::Majority))
        .max_await_time(Duration::from_millis(5))
        .batch_size(10)
        .build();
    let result = coll.watch(pipeline, Some(options)).await.unwrap();
    assert_eq!(
        result.options().unwrap().selection_criteria.unwrap(),
        SelectionCriteria::ReadPreference(ReadPreference::Primary)
    );
    assert_eq!(
        result.options().unwrap().read_concern.unwrap(),
        ReadConcern::from(ReadConcernLevel::Majority)
    );
}
