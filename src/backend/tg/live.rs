//! Gli update live di Telegram tradotti negli eventi neutri `Live`.

use grammers_client::session::types::PeerId;
use grammers_client::tl;
use grammers_client::update::Update;
use grammers_client::Client;
use tokio::sync::mpsc;

use super::{dialog_from_message, reactions_from, row_from_message, seen_from_raw, sender_name, to_chat_id, typing_label};
use crate::model::{DialogInfo, Live};
use crate::send::Response;

/// La voce di lista della chat del messaggio, solo se il peer è già nel
/// pacchetto dell'update (niente rete: un messaggio da una chat mai vista
/// senza peer fa rileggere la lista dialoghi all'app).
async fn cached_dialog(client: &Client, m: &grammers_client::message::Message) -> Option<DialogInfo> {
    m.peer()?;
    dialog_from_message(client, m).await
}

/// Un update grammers come eventi neutri (vuoto: non interessa all'app).
pub async fn translate(client: &Client, update: Update) -> Vec<Live> {
    match update {
        Update::NewMessage(m) => {
            let m = m.into_inner();
            let dialog = cached_dialog(client, &m).await;
            vec![Live::NewMessage {
                chat: to_chat_id(m.peer_id()),
                row: row_from_message(&m, sender_name(&m)),
                dialog,
            }]
        }
        Update::MessageEdited(m) => {
            let m = m.into_inner();
            vec![Live::Edited {
                chat: to_chat_id(m.peer_id()),
                row: row_from_message(&m, sender_name(&m)),
            }]
        }
        Update::MessageDeleted(deletion) => {
            // Gli id dei messaggi nei canali/supergroup sono per-canale e
            // collidono con quelli delle chat personali: senza canale la
            // cancellazione vale per le chat con id condivisi.
            vec![Live::Deleted {
                chat: deletion.channel_id().and_then(PeerId::channel).map(to_chat_id),
                ids: deletion.messages().to_vec(),
            }]
        }
        Update::Raw(raw) => {
            let chat = |p: tl::enums::Peer| to_chat_id(PeerId::from(p));
            let channel = |id: i64| PeerId::channel(id).map(to_chat_id);
            let user = |id: i64| PeerId::user(id).map(to_chat_id);
            let small = |id: i64| PeerId::chat(id).map(to_chat_id);
            let ev = match raw.raw {
                // le reazioni arrivano come update grezzo: aggiorna la riga
                tl::enums::Update::MessageReactions(u) => Some(Live::Reactions {
                    items: reactions_from(&u.reactions),
                    chat: chat(u.peer),
                    msg: u.msg_id,
                }),
                // letti da un'altra sessione (telefono, web): il badge segue
                tl::enums::Update::ReadHistoryInbox(u) if u.top_msg_id.is_none() => {
                    Some(Live::ReadInbox { chat: chat(u.peer), still: u.still_unread_count })
                }
                tl::enums::Update::ReadChannelInbox(u) => {
                    channel(u.channel_id).map(|chat| Live::ReadInbox { chat, still: u.still_unread_count })
                }
                tl::enums::Update::PeerBlocked(u) => {
                    Some(Live::Blocked { chat: chat(u.peer_id), blocked: u.blocked })
                }
                // le spunte doppie: l'altro ha letto fino a max_id
                tl::enums::Update::ReadHistoryOutbox(u) => {
                    Some(Live::ReadOutbox { chat: chat(u.peer), max_id: u.max_id })
                }
                tl::enums::Update::ReadChannelOutbox(u) => {
                    channel(u.channel_id).map(|chat| Live::ReadOutbox { chat, max_id: u.max_id })
                }
                tl::enums::Update::UserStatus(u) => {
                    user(u.user_id).map(|chat| Live::Seen { chat, seen: seen_from_raw(&u.status) })
                }
                tl::enums::Update::UserTyping(u) => user(u.user_id).map(|chat| Live::Typing {
                    chat,
                    who: None,
                    what: typing_label(&u.action),
                }),
                tl::enums::Update::ChatUserTyping(u) => small(u.chat_id).map(|c| Live::Typing {
                    chat: c,
                    who: Some(chat(u.from_id)),
                    what: typing_label(&u.action),
                }),
                tl::enums::Update::ChannelUserTyping(u) => channel(u.channel_id).map(|c| Live::Typing {
                    chat: c,
                    who: Some(chat(u.from_id)),
                    what: typing_label(&u.action),
                }),
                _ => None,
            };
            ev.into_iter().collect()
        }
        _ => Vec::new(),
    }
}

/// Lo stream degli update live: ogni evento tradotto va sul canale delle
/// risposte, come quelle dei worker. Finisce quando l'app chiude il canale.
pub async fn run(
    client: Client,
    updates: mpsc::Receiver<grammers_client::session::updates::UpdatesLike>,
    out: mpsc::UnboundedSender<Response>,
) -> Result<(), String> {
    let cfg = grammers_client::sender::UpdatesConfiguration { catch_up: false };
    let mut stream = client
        .stream_updates(updates, cfg)
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(async move {
        loop {
            let sent = match stream.next().await {
                Ok(u) => translate(&client, u)
                    .await
                    .into_iter()
                    .all(|ev| out.send(Response::Live(ev)).is_ok()),
                Err(e) => out.send(Response::LiveErr(e.to_string())).is_ok(),
            };
            if !sent {
                return;
            }
        }
    });
    Ok(())
}
