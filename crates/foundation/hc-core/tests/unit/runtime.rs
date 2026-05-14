use super::*;

#[test]
fn broadcast_reaches_all_instances_in_session() {
    let mut runtime = RuntimeSupervisor::new();
    let session = runtime.create_session("demo");
    let alice = runtime
        .create_instance(&session.id, "alice", None)
        .expect("alice should be created");
    let bob = runtime
        .create_instance(&session.id, "bob", None)
        .expect("bob should be created");

    runtime
        .post_message(
            &session.id,
            &alice.id,
            MessageRoute::Broadcast,
            MessageKind::Chat,
            "hello all",
            None,
        )
        .expect("broadcast should succeed");

    let alice_mailbox = runtime
        .mailbox_for_instance(&session.id, &alice.id)
        .expect("alice mailbox should load");
    let bob_mailbox = runtime
        .mailbox_for_instance(&session.id, &bob.id)
        .expect("bob mailbox should load");

    assert_eq!(alice_mailbox.len(), 1);
    assert_eq!(bob_mailbox.len(), 1);
    assert_eq!(alice_mailbox[0].body, "hello all");
    assert_eq!(bob_mailbox[0].body, "hello all");
}

#[test]
fn channel_message_only_reaches_subscribed_instances() {
    let mut runtime = RuntimeSupervisor::new();
    let session = runtime.create_session("demo");
    let alice = runtime
        .create_instance(&session.id, "alice", None)
        .expect("alice should be created");
    let bob = runtime
        .create_instance(&session.id, "bob", None)
        .expect("bob should be created");
    let carol = runtime
        .create_instance(&session.id, "carol", None)
        .expect("carol should be created");
    let channel = runtime
        .create_channel(&session.id, "planning")
        .expect("channel should be created");

    runtime
        .join_channel(&alice.id, &channel.id)
        .expect("alice should join");
    runtime
        .join_channel(&bob.id, &channel.id)
        .expect("bob should join");

    runtime
        .post_message(
            &session.id,
            &alice.id,
            MessageRoute::Channel {
                channel_id: channel.id.clone(),
            },
            MessageKind::Chat,
            "plan update",
            None,
        )
        .expect("channel message should succeed");

    let alice_mailbox = runtime
        .mailbox_for_instance(&session.id, &alice.id)
        .expect("alice mailbox should load");
    let bob_mailbox = runtime
        .mailbox_for_instance(&session.id, &bob.id)
        .expect("bob mailbox should load");
    let carol_mailbox = runtime
        .mailbox_for_instance(&session.id, &carol.id)
        .expect("carol mailbox should load");

    assert_eq!(alice_mailbox.len(), 1);
    assert_eq!(bob_mailbox.len(), 1);
    assert!(carol_mailbox.is_empty());
    assert_eq!(
        alice_mailbox[0].route,
        MessageRoute::Channel {
            channel_id: channel.id.clone()
        }
    );
    assert_eq!(
        bob_mailbox[0].route,
        MessageRoute::Channel {
            channel_id: channel.id.clone()
        }
    );
}

#[test]
fn runtime_namespace_propagates_from_session_to_instances_and_channels() {
    let mut runtime = RuntimeSupervisor::new();
    let session =
        runtime.create_session_in_namespace("demo", RuntimeNamespace::new("tenant-a", "user-a"));
    let instance = runtime
        .create_instance(&session.id, "alice", None)
        .expect("instance should be created");
    let channel = runtime
        .create_channel(&session.id, "planning")
        .expect("channel should be created");

    assert_eq!(session.namespace.tenant_id, "tenant-a");
    assert_eq!(session.namespace.user_id, "user-a");
    assert_eq!(instance.namespace, session.namespace);
    assert_eq!(channel.namespace, session.namespace);
}
