use super::*;

#[test]
fn status_expires_after_four_seconds_and_the_hints_come_back() {
    let mut a = app();
    a.advance(ms(1_000));
    a.status = Some("started asf-3".into());
    assert!(footer_line(&a, 100).contains("started asf-3"));
    a.advance(ms(1_000)); // the first tick after the message stamps it
    a.advance(ms(1_000) + STATUS_TTL - ms(1));
    assert!(a.status.is_some(), "still within the 4 s");
    a.advance(ms(1_000) + STATUS_TTL);
    assert!(a.status.is_none());
    let f = footer_line(&a, 100);
    assert!(f.contains("? help") && !f.contains("started asf-3"), "{f:?}");
}

#[test]
fn status_goes_away_on_the_next_key_press_but_a_key_may_set_a_new_one() {
    let mut a = app();
    a.status = Some("sent to asf-2".into());
    a.handle_key(key('j'));
    assert!(a.status.is_none());
    a.handle_key(key('j')); // asf-2, in progress
    a.handle_key(key('s'));
    assert!(
        a.status.as_deref().unwrap_or("").contains("only ready"),
        "{:?}",
        a.status
    );
    a.handle_key(key('k'));
    assert!(a.status.is_none(), "cleared again by the following key");
}
