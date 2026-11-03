use super::*;
use crate::domain::{Recurrence, User, UserTz};
use chrono::SubsecRound;

async fn pending(db: &Db, now: DateTime<Utc>) -> Nagging {
    db.upsert_user(User {
        id: 1,
        first_name: "Ada".into(),
        last_name: String::new(),
        tz: UserTz::parse("UTC").unwrap(),
    })
    .await
    .unwrap();
    let reminder = db
        .add_reminder(1, "proof".into(), now, Recurrence::Once)
        .await
        .unwrap();
    db.fire_reminder(reminder.id, now, now)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn only_one_claim_wins_and_snooze_invalidates_its_completion() {
    let db = Db::open_in_memory().unwrap();
    let now = Utc::now().trunc_subsecs(0);
    let occurrence = pending(&db, now).await;
    let lease = now + Duration::seconds(30);
    let (a, b) = tokio::join!(
        db.claim_delivery(1, now, lease),
        db.claim_delivery(1, now, lease)
    );
    let claims: Vec<_> = [a.unwrap(), b.unwrap()].into_iter().flatten().collect();
    assert_eq!(claims.len(), 1);
    let until = now + Duration::hours(1);
    db.snooze_nagging(1, occurrence.id, until).await.unwrap();
    db.finish_delivery(claims.into_iter().next().unwrap(), Delivery::Sent, now)
        .await
        .unwrap();
    assert_eq!(db.naggings_for_user(1).await.unwrap()[0].next_at, until);
    assert!(db.claim_delivery(1, now, lease).await.unwrap().is_none());
}

#[tokio::test]
async fn restart_recovers_an_expired_claim_and_ignores_the_old_completion() {
    let path = std::env::temp_dir().join(format!(
        "napominalochka-delivery-{}.sqlite",
        new_token().unwrap()
    ));
    let now = Utc::now().trunc_subsecs(0);
    let lease = now + Duration::seconds(30);
    let old = {
        let db = Db::open(&path).unwrap();
        pending(&db, now).await;
        db.claim_delivery(1, now, lease).await.unwrap().unwrap()
    };
    {
        let db = Db::open(&path).unwrap();
        assert!(db.claim_delivery(1, now, lease).await.unwrap().is_none());
        let recovered = db
            .claim_delivery(1, lease, lease + Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(recovered.token, old.token);
        db.finish_delivery(old, Delivery::Sent, lease)
            .await
            .unwrap();
        assert!(
            db.claim_delivery(1, lease, lease + Duration::seconds(30))
                .await
                .unwrap()
                .is_none()
        );
        db.finish_delivery(recovered, Delivery::Failed("network".into()), lease)
            .await
            .unwrap();
    }
    {
        let db = Db::open(&path).unwrap();
        assert!(
            db.claim_delivery(
                1,
                lease + Duration::seconds(59),
                lease + Duration::seconds(90)
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(
            db.claim_delivery(
                1,
                lease + Duration::minutes(1),
                lease + Duration::minutes(2)
            )
            .await
            .unwrap()
            .is_some()
        );
    }
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn late_unreachable_after_snooze_preserves_new_nags_and_checks() {
    let db = Db::open_in_memory().unwrap();
    let now = Utc::now().trunc_subsecs(0);
    let occurrence = pending(&db, now).await;
    let old = db
        .claim_delivery(1, now, now + Duration::seconds(30))
        .await
        .unwrap()
        .unwrap();
    db.snooze_nagging(1, occurrence.id, now + Duration::hours(1))
        .await
        .unwrap();
    db.set_preferences(1, None, None, Some(20)).await.unwrap();
    let completed = pending(&db, now).await;
    db.complete_nagging(1, completed.id, now).await.unwrap();
    pending(&db, now).await;

    db.finish_delivery(old, Delivery::Unreachable, now)
        .await
        .unwrap();

    assert_eq!(db.naggings_for_user(1).await.unwrap().len(), 2);
    assert_eq!(
        db.due_confirmations(now + Duration::minutes(21))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn late_unreachable_from_an_expired_claim_cannot_cancel_a_new_claim() {
    let db = Db::open_in_memory().unwrap();
    let now = Utc::now().trunc_subsecs(0);
    pending(&db, now).await;
    let expires = now + Duration::seconds(30);
    let old = db.claim_delivery(1, now, expires).await.unwrap().unwrap();
    let recovered = db
        .claim_delivery(1, expires, expires + Duration::seconds(30))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(old.token, recovered.token);

    db.finish_delivery(old, Delivery::Unreachable, expires)
        .await
        .unwrap();

    assert_eq!(db.naggings_for_user(1).await.unwrap().len(), 1);
    assert!(
        db.claim_delivery(1, expires, expires + Duration::seconds(30))
            .await
            .unwrap()
            .is_none()
    );
    db.finish_delivery(recovered, Delivery::Sent, expires)
        .await
        .unwrap();
    assert_eq!(
        db.naggings_for_user(1).await.unwrap()[0].next_at,
        expires + Duration::minutes(10)
    );
}
