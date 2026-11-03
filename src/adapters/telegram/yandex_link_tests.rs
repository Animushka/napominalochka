use super::yandex_receipt;

#[test]
fn accepts_only_exact_bot_start_receipts() {
    let receipt = "a".repeat(48);
    assert_eq!(
        yandex_receipt(&format!("/start yandex_{receipt}"), "our_bot"),
        Some(receipt.as_str())
    );
    assert_eq!(
        yandex_receipt(&format!("/start@our_bot yandex_{receipt}"), "our_bot"),
        Some(receipt.as_str())
    );
    for text in [
        format!("/start@other_bot yandex_{receipt}"),
        format!("/start yandex_{receipt} extra"),
        "/start yandex_nothex".into(),
        "/start".into(),
    ] {
        assert_eq!(yandex_receipt(&text, "our_bot"), None);
    }
}
