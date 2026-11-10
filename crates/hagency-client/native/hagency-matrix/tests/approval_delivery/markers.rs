use super::*;

#[tokio::test]
async fn native_private_approval_marker_is_written_before_card_and_replayed_without_mutation() {
    let mut f = Fixture::new().await;
    f.enroll().await.unwrap();
    let first_card = f.card(1, true).await;
    assert_eq!(
        f.send(first_card).await.unwrap().state,
        PrivateApprovalDeliveryState::Accepted
    );
    assert_eq!(f.peer.events.len(), 1);
    assert_eq!(f.peer.approval_markers.len(), 1);
    let first = f.peer.approval_markers[0].clone();
    assert_eq!(first["sender"], BOT);
    assert_eq!(first["room_id"], ROOM);
    assert_eq!(first["type"], "com.agentchat.approval.room.v2");
    assert_eq!(first["state_key"], "");
    assert_eq!(
        first["content"],
        json!({"version":2,"binding_generation":1,
        "publisher_mxid":BOT,"owner_mxid":crypto::HUMAN,
        "project_room_associations":[{"agent":"Worker","project_room_id":"!project:example.test","active":true}]})
    );
    drive(
        f.collector
            .prepare_private_approval_rooms(&CancellationToken::new()),
        &mut f.fake,
        &mut f.peer,
    )
    .await
    .unwrap();
    assert_eq!(f.peer.approval_markers, vec![first.clone()]);
    // A previous binding set must be replaced using only current stored
    // associations and a higher generation, never copied from room content.
    f.peer.approval_markers[0]["content"]["project_room_associations"]
        .as_array_mut()
        .unwrap()
        .push(json!({"agent":"retired","project_room_id":"!retired:example.test","active":false}));
    drive(
        f.collector
            .prepare_private_approval_rooms(&CancellationToken::new()),
        &mut f.fake,
        &mut f.peer,
    )
    .await
    .unwrap();
    assert_eq!(f.peer.approval_markers.len(), 2);
    let mut next = first["content"].clone();
    next["binding_generation"] = json!(2);
    assert_eq!(f.peer.approval_markers[1]["content"], next);
    assert_eq!(f.peer.events.len(), 1);
    let card = f.card(2, true).await;
    assert_eq!(
        f.send(card).await.unwrap().state,
        PrivateApprovalDeliveryState::Accepted
    );
    assert_eq!(f.peer.events.len(), 2);
    assert_eq!(f.peer.approval_markers.len(), 2);
    f.close().await;
}

#[tokio::test]
async fn native_private_approval_marker_refuses_foreign_state_and_swapped_acknowledgement() {
    for wrong in [
        "sender",
        "event_id",
        "room_id",
        "private_members",
        "read_refused",
    ] {
        let mut f = Fixture::new().await;
        let result = drive_with(f.collector.prepare_private_approval_rooms(&CancellationToken::new()), &mut f.fake, &mut f.peer,
            |request, _, response| {
                if request.target.contains("/event/") {
                    match wrong {
                        "sender" => response.1["sender"] = json!("@other:example.test"),
                        "event_id" => response.1["event_id"] = json!("$swapped"),
                        "room_id" => response.1["room_id"] = json!("!elsewhere:example.test"),
                        _ => {}
                    }
                } else if wrong == "read_refused" && request.target.ends_with("/state") {
                    *response = (503, json!({"errcode":"M_UNKNOWN","error":"temporarily unavailable"}));
                } else if wrong == "private_members" && request.target.ends_with("/state") {
                    response.1.as_array_mut().unwrap().push(json!({"type":"m.room.member","state_key":"@other:example.test","content":{"membership":"join"}}));
                }
            }).await;
        assert!(result.is_err(), "accepted {wrong}");
        assert!(
            f.peer.events.is_empty(),
            "no card may follow a refused marker"
        );
        if wrong == "private_members" {
            assert!(f.peer.approval_markers.is_empty());
        }
        if wrong == "read_refused" {
            // A confirmed pre-write read refusal can recover without releasing
            // any unknown transmission or changing the last safe observation.
            drive(
                f.collector
                    .prepare_private_approval_rooms(&CancellationToken::new()),
                &mut f.fake,
                &mut f.peer,
            )
            .await
            .unwrap();
            assert_eq!(f.peer.approval_markers.len(), 1);
        } else if wrong == "private_members" {
            // Negative membership changes the stored observation. A different
            // snapshot at the same generation must remain fenced.
            assert_eq!(
                drive(
                    f.collector
                        .prepare_private_approval_rooms(&CancellationToken::new()),
                    &mut f.fake,
                    &mut f.peer
                )
                .await,
                Err(Error::Conflict)
            );
            assert!(f.peer.approval_markers.is_empty());
        } else {
            // An uncertain original publication cannot silently re-enter writes.
            assert!(
                f.collector
                    .prepare_private_approval_rooms(&CancellationToken::new())
                    .await
                    .is_err()
            );
        }
        let _ = f.collector.close().await;
        common::shutdown_domain(&f.base.store, "marker-refusal").await;
        f.fake.close().await;
    }
}

#[tokio::test]
async fn native_private_approval_marker_write_refusal_is_retried_and_an_unknown_outcome_stays_fenced() {
    for (status, refusal, retried) in [
        (403, Error::Unauthorized, true),
        (400, Error::Remote(400), true),
        // The homeserver may have stored the event behind a failing reply.
        (503, Error::Remote(503), false),
    ] {
        let mut f = Fixture::new().await;
        let result = drive_with(
            f.collector
                .prepare_private_approval_rooms(&CancellationToken::new()),
            &mut f.fake,
            &mut f.peer,
            |request, peer, response| {
                if request.method == "PUT"
                    && request.target.contains("/state/com.agentchat.approval.room.v2")
                {
                    // The homeserver stored nothing.
                    peer.approval_markers.pop();
                    *response = (status, json!({"errcode":"M_FORBIDDEN","error":"refused"}));
                }
            },
        )
        .await;
        assert_eq!(result, Err(refusal.clone()), "status {status}");
        assert!(f.peer.approval_markers.is_empty());
        if retried {
            // A refused write made no mutation: the same collector publishes
            // the marker once the homeserver accepts it, without a restart.
            drive(
                f.collector
                    .prepare_private_approval_rooms(&CancellationToken::new()),
                &mut f.fake,
                &mut f.peer,
            )
            .await
            .unwrap();
            assert_eq!(f.peer.approval_markers.len(), 1);
            assert_eq!(f.peer.approval_markers[0]["content"]["binding_generation"], 1);
        } else {
            assert_eq!(
                f.collector
                    .prepare_private_approval_rooms(&CancellationToken::new())
                    .await,
                Err(refusal),
                "an uncertain publication cannot silently re-enter writes"
            );
            assert!(f.peer.approval_markers.is_empty());
        }
        let _ = f.collector.close().await;
        common::shutdown_domain(&f.base.store, "marker-write-refusal").await;
        f.fake.close().await;
    }
}
