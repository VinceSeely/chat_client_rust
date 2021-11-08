// #![deny(warnings)]
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use futures_util::{SinkExt, StreamExt, TryFutureExt};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::ws::{Message, WebSocket};
use warp::Filter;
use std::io::BufWriter;
use std::io::Write;
use std::fs::OpenOptions;

/// Our global unique user id counter.
static NEXT_USER_ID: AtomicUsize = AtomicUsize::new(1);

/// Our state of currently connected users.
///
/// - Key is their id
/// - Value is a sender of `warp::ws::Message`
type Users = Arc<RwLock<HashMap<usize, mpsc::UnboundedSender<Message>>>>;
type UsersRoom = Arc<RwLock<HashMap<usize, String>>>;

#[tokio::main]
async fn main() {

    // Keep track of all connected users, key is usize, value
    // is a websocket sender.
    let users = Users::default();
    // Turn our "state" into a new Filter...
    let users = warp::any().map(move || users.clone());
    let user_rooms : UsersRoom = UsersRoom::default();
    let user_rooms = warp::any().map(move || user_rooms.clone());

    // GET /chat -> websocket upgrade
    let chat = warp::path!("chat" / String)
        // The `ws()` filter will prepare Websocket handshake...
        .and(warp::ws())
        .and(users)
        .and(user_rooms)
        .map(|room: String, ws: warp::ws::Ws, users, room_map | {
            
            // This will call our function if the handshake succeeds.
            ws.on_upgrade(move |socket| user_connected(socket, users, room, room_map))
        });

    // GET / -> index html
    let index = warp::path!("chatroom" / String).map(|s: String| warp::reply::html(str::replace(INDEX_HTML, "{}", &s)));

    let routes = index.or(chat);

    warp::serve(routes).run(([127, 0, 0, 1], 3030)).await;
}

async fn user_connected(ws: WebSocket, users: Users, room: String, room_map: UsersRoom) {
    // Use a counter to assign a new unique ID for this user.
    let my_id = NEXT_USER_ID.fetch_add(1, Ordering::Relaxed);

    eprintln!("new chat user room {}", room);

    eprintln!("new chat user: {}", my_id);

    // Split the socket into a sender and receive of messages.
    let (mut user_ws_tx, mut user_ws_rx) = ws.split();

    // Use an unbounded channel to handle buffering and flushing of messages
    // to the websocket...
    let (tx, rx) = mpsc::unbounded_channel();
    let mut rx = UnboundedReceiverStream::new(rx);

    tokio::task::spawn(async move {
        while let Some(message) = rx.next().await {
            user_ws_tx
                .send(message)
                .unwrap_or_else(|e| {
                    eprintln!("websocket send error: {}", e);
                })
                .await;
        }
    });

    // Save the sender in our list of connected users.
    users.write().await.insert(my_id, tx);
    room_map.write().await.insert(my_id, room.to_string());
    // Return a `Future` that is basically a state machine managing
    // this specific user's connection.

    // Every time the user sends a message, broadcast it to
    // all other users...
    while let Some(result) = user_ws_rx.next().await {
        
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("websocket error(uid={}): {}", my_id, e);
                break;
            }
        };
        
        user_message(my_id, msg, &users, &room, &room_map).await;
        
    }

    // user_ws_rx stream will keep processing as long as the user stays
    // connected. Once they disconnect, then...
    user_disconnected(my_id, &users, &room_map).await;
}

async fn user_message(my_id: usize, msg: Message, users: &Users, room: &String, room_map: &UsersRoom) {
    // Skip any non-Text messages...
    let msg = if let Ok(s) = msg.to_str() {
        s
    } else {
        return;
    };

    let new_msg = format!("<User#{}>: {}", my_id, msg);

    write_to_file(&new_msg, &room);

    // New message from this user, send it to everyone else (except same uid)...
    for (&uid, tx) in users.read().await.iter() {
        if my_id != uid && room_active(&uid, &room, &room_map).await {
            if let Err(_disconnected) = tx.send(Message::text(new_msg.clone())) {
                // The tx is disconnected, our `user_disconnected` code
                // should be happening in another task, nothing more to
                // do here.
            }
        }
    }
}

async fn room_active(uid: &usize, room: &String, room_map: &UsersRoom) -> bool {
    room.to_string() == room_map.read().await.get(&uid).unwrap().to_string()
}

fn write_to_file(message: &String, room: &String){
    let f = OpenOptions::new()
        .write(true)
        .append(true)
        .open(room)
        .expect("unable to open file");
    
    let mut buf = BufWriter::new(f);
    let new_message = format!("{}\n", message);
    buf.write_all(new_message.as_bytes()).expect("Unable to write data");
}

async fn user_disconnected(my_id: usize, users: &Users, room_map: &UsersRoom) {
    eprintln!("good bye user: {}", my_id);

    // Stream closed up, so remove from the user list
    users.write().await.remove(&my_id);
    room_map.write().await.remove(&my_id);
}

static INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
    <head>
        <title>Warp Chat {}</title>
    </head>
    <body>
        <h1>Warp chat {}</h1>
        <div id="chat">
            <p><em>Connecting...</em></p>
        </div>
        <input type="text" id="text" />
        <button type="button" id="send">Send</button>
        <script type="text/javascript">
            const chat = document.getElementById('chat');
            const text = document.getElementById('text');
            const uri = 'ws://' + location.host + '/chat/{}';
            const ws = new WebSocket(uri);

            function message(data) {
                const line = document.createElement('p');
                line.innerText = data;
                chat.appendChild(line);
            }

            ws.onopen = function() {
                chat.innerHTML = '<p><em>Connected!</em></p>';
            };

            ws.onmessage = function(msg) {
                message(msg.data);
            };

            ws.onclose = function() {
                chat.getElementsByTagName('em')[0].innerText = 'Disconnected!';
            };

            send.onclick = function() {
                const msg = text.value;
                ws.send(msg, '{}');
                text.value = '';

                message('<You>: ' + msg);
            };
        </script>
    </body>
</html>
"#;


#[tokio::test]
async fn room_message_to_user() {

    let room_map = UsersRoom::default();
    let id: usize = 12;
    let room: String = "valid_room".to_string();
    
    room_map.write().await.insert(id, room.to_string());


    assert_eq!(room_active(&id, &room, &room_map).await, true);

}

#[tokio::test]
async fn room_message_to_user_not_in_room() {

    let room_map = UsersRoom::default();
    let wrong_id: usize = 12;
    let room: String = "valid_room".to_string();
    room_map.write().await.insert(wrong_id, room.to_string());
    
    let id: usize = 13;
    let room: String = "different_room".to_string();
    room_map.write().await.insert(id, room.to_string());


    assert_eq!(room_active(&wrong_id, &room, &room_map).await, false);

}