//! pa13a: the front desk's bot turns. A guest's message on a desk pocket
//! ([`crate::pocket::desk`]) is answered by the host's bot as an outsider turn, the same runtime
//! as a Telegram customer's ([`super::outsider_turns`]): one thread per guest, the messages saved
//! as an outsider's (`meta.source = "pocket-guest"`, which `crate::drivers` frames like a
//! customer's), the note with the pack's never rules and the host's facts, every tool call asking
//! the host, one turn at a time per guest (what comes meanwhile goes in the next turn), and only
//! the reply's plain text back into that guest's view.
//!
//! A reply that starts with [`super::outsider_turns::HANDOFF_MARK`] hands the guest to the host:
//! the guest gets a `handoff` note, the host a Needs-you item ([`needs_you`]) and a notification
//! with no guest words in it. The host's `reply` goes into the guest's view and the bot's thread.
//!
//! Nothing a guest wrote, their name or a reply goes into a log line or an event beyond the
//! host's own view (the thread is the host's).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use serde_json::{json, Value};

use super::outsider_turns::{self as outsider, NoteWords};
use super::routes::TurnOpts;
use super::AppState;
use crate::db::models::Kwant;
use crate::drivers::events::RuntimeEvent;
use crate::pocket::{self, desk, kits::Viewer};

/// A guest turn's purpose (`TurnOpts::purpose`, and on its cards' chat).
pub const PURPOSE: &str = "pocket_guest";
/// What started the turn, on its approval cards.
pub const STARTED_BY: &str = "Guest page";
pub const SOURCE: &str = crate::drivers::POCKET_GUEST_SOURCE;

/// Every call in a guest's turn asks the host, whatever the approvals say.
pub(in crate::server) fn guest_opts(opts: &mut TurnOpts) {
    outsider::outsider_opts(opts, STARTED_BY, PURPOSE);
}

const NOTE_WORDS: NoteWords<'static> = NoteWords {
    who: ", a guest of the host's place, writing on the host's guest page",
    answer_from: "the host's facts below",
    hand_off_when: "one of the rules below, a booking, a refund, a late check-out, a complaint, anything the host has to decide",
    reason: "a short label for the host first (like \"Late check-out: 2 PM, Sun Dec 8\")",
    then: "that the host will reply",
    sender: "guest",
};

/// What a front desk bot never does on its own, whatever the pack says.
pub const DESK_NEVER: &str = "Confirm a booking, a refund, a late check-out or an early check-in yourself";

/// The note after a guest's messages: the pack's never rules and the desk's own, then the
/// host's house guide.
pub fn desk_note(name: &str, never: &[String], guide: &str) -> String {
    let mut rules = never.to_vec();
    rules.push(DESK_NEVER.to_string());
    outsider::outsider_note(name, &NOTE_WORDS, &rules, Some(("The host's facts (the house guide):", guide)))
}

type Key = (PathBuf, String, String);

/// Guests with a worker running, per kwant data folder (tests run several servers at once).
static WORKERS: LazyLock<Mutex<HashSet<Key>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn key(state: &AppState, code: &str, guest: &str) -> Key {
    (state.config.data_dir.clone(), code.to_string(), guest.to_string())
}

/// Starts this guest's worker unless one runs.
pub fn kick(state: &AppState, code: &str, guest: &str) {
    let k = key(state, code, guest);
    if !WORKERS.lock().unwrap_or_else(|e| e.into_inner()).insert(k.clone()) {
        return;
    }
    let (state, code, guest) = (state.clone(), code.to_string(), guest.to_string());
    tokio::spawn(async move {
        let drained = worker(&state, &code, &guest).await;
        WORKERS.lock().unwrap_or_else(|e| e.into_inner()).remove(&k);
        // A message that came between the last take and now starts the next round
        if drained && still_busy(&state, &code, &guest).await {
            kick(&state, &code, &guest);
        }
    });
}

async fn still_busy(state: &AppState, code: &str, guest: &str) -> bool {
    let Ok(Some(s)) = pocket::snapshot(state, code).await else { return false };
    s.row.state == crate::db::pocket::OPEN && s.players.iter().any(|p| p.id == guest) && desk::busy_guests(&s.kit_state).iter().any(|g| g == guest)
}

/// Runs this guest's turns until nothing waits. `true` when it stopped because nothing did.
async fn worker(state: &AppState, code: &str, guest: &str) -> bool {
    loop {
        let Ok(taken) = pocket::server_event(state, code, "take", &json!({ "guest": guest })).await else { return false };
        if taken["capped"] == json!(true) {
            told_host(state, code, guest).await;
        }
        let texts: Vec<String> = taken["texts"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        if texts.is_empty() {
            return true;
        }
        if !run_one(state, code, guest, texts, taken["thread_id"].as_str()).await {
            return false;
        }
    }
}

/// One turn for one guest's messages. `false` when the worker must stop (the pocket, the guest
/// or the bot is gone).
async fn run_one(state: &AppState, code: &str, guest: &str, texts: Vec<String>, thread: Option<&str>) -> bool {
    let Ok(Some(s)) = pocket::snapshot(state, code).await else { return false };
    let Some(player) = s.players.iter().find(|p| p.id == guest).cloned() else { return false };
    let bot = s.spec["answer_bot"].as_str().unwrap_or("");
    let Ok(Some(kwant)) = state.db.get_kwant(bot).await else {
        let _ = pocket::server_event(state, code, "handoff", &json!({ "guest": guest, "why": "The bot that answers guests was deleted." })).await;
        let _ = pocket::server_event(state, code, "idle", &json!({ "guest": guest })).await;
        return false;
    };
    let thread_id = match thread {
        Some(t) => t.to_string(),
        None => match open_thread(state, code, guest, &kwant, &player.name).await {
            Some(t) => t,
            None => {
                let _ = pocket::server_event(state, code, "idle", &json!({ "guest": guest })).await;
                return false;
            }
        },
    };
    let never = outsider::never_rules(state, &kwant.space_id, &kwant.id).await;
    let turn = outsider::OutsiderTurn {
        kwant: &kwant,
        thread_id: &thread_id,
        sender_id: format!("{SOURCE}:{code}:{guest}"),
        meta: json!({ "source": SOURCE, "from_customer": player.name, "pocket_code": code, "guest_id": guest }),
        texts: &texts,
        note: desk_note(&player.name, &never, desk::guide(&s.spec)),
        opts: guest_opts,
        extra_meta: json!({ "pocket_code": code, "guest_reply": true }),
    };
    let Some(result) = outsider::run_outsider_turn(state, turn).await else {
        let _ = pocket::server_event(state, code, "idle", &json!({ "guest": guest })).await;
        return false;
    };
    let step = match result {
        Ok(reply) => match outsider::handoff_reason(&reply, desk::REASON_MAX) {
            Some(why) => hand_off(state, code, guest, &why).await,
            None => {
                let app_url = outsider::app_url(state, &kwant.space_id).await;
                match outsider::plain_reply(&reply, app_url.as_deref()) {
                    Some(out) => pocket::server_event(state, code, "answer", &json!({ "guest": guest, "text": out })).await.map(|_| ()),
                    None => hand_off(state, code, guest, "The bot's reply had nothing it could send.").await,
                }
            }
        },
        Err(_) => hand_off(state, code, guest, "The bot couldn't answer this message.").await,
    };
    match step {
        Ok(()) => true,
        Err(e) => {
            // The guest or the pocket went while the bot worked: their conversation goes too
            if matches!(e.code, "no_such_player" | "not_found") {
                let _ = state.db.delete_thread_with_messages(&thread_id).await;
            }
            false
        }
    }
}

async fn open_thread(state: &AppState, code: &str, guest: &str, kwant: &Kwant, name: &str) -> Option<String> {
    let thread = state.db.create_thread(&kwant.id, name, "chat", None, SOURCE).await.ok()?;
    let _ = state.event_sender.send(RuntimeEvent::ThreadUpdated { thread: serde_json::to_value(&thread).unwrap_or(Value::Null) });
    if pocket::server_event(state, code, "thread", &json!({ "guest": guest, "thread_id": thread.id })).await.is_err() {
        let _ = state.db.delete_thread_with_messages(&thread.id).await;
        return None;
    }
    Some(thread.id)
}

/// The bot can't or mustn't answer: the guest gets the `handoff` note, the host a Needs-you item
/// and a notification.
async fn hand_off(state: &AppState, code: &str, guest: &str, why: &str) -> pocket::PResult<()> {
    pocket::server_event(state, code, "handoff", &json!({ "guest": guest, "why": why })).await?;
    told_host(state, code, guest).await;
    Ok(())
}

/// Tells the host a guest waits for them. Never the guest's words or the reason: the body may
/// reach a phone service.
async fn told_host(state: &AppState, code: &str, guest: &str) {
    let Ok(Some(s)) = pocket::snapshot(state, code).await else { return };
    let Some(player) = s.players.iter().find(|p| p.id == guest) else { return };
    let Ok(Some(kwant)) = state.db.get_kwant(s.spec["answer_bot"].as_str().unwrap_or("")).await else { return };
    super::continuity::notify(
        state,
        super::continuity::Notification {
            kind: "question_asked",
            kwant_id: kwant.id.clone(),
            kwant_name: kwant.name.clone(),
            channel_id: None,
            title: format!("{} needs you", player.name),
            body: format!("A guest on {} is waiting for your reply.", s.row.title),
            thread_id: desk::thread_of(&s.kit_state, guest),
            card_id: None,
        },
    )
    .await;
}

/// After an action on a desk pocket: a guest's message starts their worker (or, past the daily
/// cap, tells the host); the host's reply goes into the bot's thread with that guest.
pub async fn after_act(state: &AppState, code: &str, actor: &Viewer, name: &str, body: &Value) {
    match (actor, name) {
        (Viewer::Guest(guest), "say") => {
            let Ok(Some(s)) = pocket::snapshot(state, code).await else { return };
            let at = desk::last_message_at(&s.kit_state, guest);
            if at.is_some_and(|at| desk::capped_at(&s.kit_state, guest, at)) {
                told_host(state, code, guest).await;
            } else if desk::busy_guests(&s.kit_state).iter().any(|g| g == guest) {
                kick(state, code, guest);
            }
        }
        (Viewer::Host, "reply") => {
            let guest = body["guest"].as_str().unwrap_or("");
            let text = body["text"].as_str().unwrap_or("").trim();
            let Ok(Some(s)) = pocket::snapshot(state, code).await else { return };
            if let Some(thread) = desk::thread_of(&s.kit_state, guest) {
                let _ = outsider::save_persons_reply(state, &thread, text, json!({ "to_guest": guest, "pocket_code": code })).await;
            }
        }
        _ => {}
    }
}

/// Re-starts the turns a restart left queued (the sweeper calls it once a minute; a running
/// worker is left alone).
pub async fn resume(state: &AppState) {
    for code in state.db.pocket_codes_open_of_kit(desk::ID).await.unwrap_or_default() {
        let Ok(Some(s)) = pocket::snapshot(state, &code).await else { continue };
        for guest in desk::busy_guests(&s.kit_state) {
            if s.players.iter().any(|p| p.id == guest) {
                kick(state, &code, &guest);
            }
        }
    }
}

/// Needs-you items: one per guest of an open desk pocket who waits for the host, for desks
/// answered by this space's bots.
pub async fn needs_you(state: &AppState, kwants: &[Kwant]) -> Vec<(chrono::DateTime<chrono::Utc>, Value)> {
    let mut items = Vec::new();
    if !pocket::enabled(&state.db).await {
        return items;
    }
    for code in state.db.pocket_codes_open_of_kit(desk::ID).await.unwrap_or_default() {
        let Ok(Some(s)) = pocket::snapshot(state, &code).await else { continue };
        let bot = s.spec["answer_bot"].as_str().unwrap_or("");
        let Some(kwant) = kwants.iter().find(|k| k.id == bot) else { continue };
        for (guest, why, quote, at, thread) in desk::waiting_guests(&s.kit_state) {
            let Some(p) = s.players.iter().find(|p| p.id == guest) else { continue };
            items.push((at, json!({
                "kind": "pocket_desk",
                "id": format!("desk:{code}:{guest}"),
                "pocket_code": code,
                "pocket_title": s.row.title,
                "guest_id": guest,
                "guest_name": p.name,
                "text": why,
                "quote": quote,
                "asked_at": at,
                "kwant_id": kwant.id,
                "kwant_name": kwant.name,
                "channel_id": null,
                "direct_kwant_id": kwant.id,
                "thread_id": thread,
                "follow_up_at": null,
                "reminded_at": null,
            })));
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desk_note_frames_the_guest_and_carries_the_guide() {
        let n = desk_note("Ann", &["Share prices".into()], "Wi-Fi: sea123");
        assert!(n.starts_with(crate::drivers::CUSTOMER_NOTE));
        assert!(n.contains("writing on the host's guest page") && n.contains("outside person"), "{n}");
        assert!(n.contains("- Share prices\n- Confirm a booking"), "{n}");
        assert!(n.contains("The host's facts (the house guide):\nWi-Fi: sea123"), "{n}");
        assert!(n.contains("that the host will reply"), "{n}");
    }
}
