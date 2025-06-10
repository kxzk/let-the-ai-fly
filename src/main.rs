use crossterm::event::{poll, read, Event, KeyCode};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time::sleep;

use controller::{Controller, ManualController};
use drone::{DroneConnection, TelloDrone};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // initialize drone connection
    let mut drone = TelloDrone::new("192.168.10.1:8889", "0.0.0.0:9000").await?;

    // connect to drone
    drone.connect().await?;

    let drone = Arc::new(Mutex::new(drone));

    // create controller (easily swappable with LLMController later)
    let controller = ManualController::new();

    // run control loop
    run_control_loop(drone, controller).await?;

    Ok(())
}

async fn run_control_loop<D, C>(drone: Arc<Mutex<D>>, controller: C) -> anyhow::Result<()>
where
    D: DroneConnection + 'static,
    C: Controller,
{
    loop {
        // get control input
        if let Some(action) = controller.get_action().await? {
            match action {
                controller::Action::Exit => break,
                controller::Action::Command(cmd) => {
                    let mut d = drone.lock().await;
                    if let Err(e) = d.send_command(&cmd).await {
                        println!("[error] command failed: {}", e);
                        // try to reconnect
                        if let Err(_) = d.ensure_connected().await {
                            println!("[error] lost connection to drone");
                        }
                    }
                }
                controller::Action::NoOp => {}
            }
        }
    }
    Ok(())
}

mod drone {
    use super::*;

    pub trait DroneConnection: Send + Sync {
        async fn connect(&mut self) -> anyhow::Result<()>;
        async fn send_command(&mut self, cmd: &str) -> anyhow::Result<()>;
        async fn ensure_connected(&mut self) -> anyhow::Result<()>;
        async fn get_video_stream(&self) -> Option<VideoStream>;
    }

    pub struct VideoStream {
        // placeholder for video stream handling
    }

    pub struct TelloDrone {
        socket: UdpSocket,
        tello_addr: String,
        connected: bool,
    }

    impl TelloDrone {
        pub async fn new(tello_addr: &str, bind_addr: &str) -> anyhow::Result<Self> {
            let socket = UdpSocket::bind(bind_addr).await?;
            socket.connect(tello_addr).await?;

            Ok(Self {
                socket,
                tello_addr: tello_addr.to_string(),
                connected: false,
            })
        }

        async fn send_with_retry(&self, cmd: &str, max_retries: u32) -> anyhow::Result<()> {
            for attempt in 0..=max_retries {
                if attempt > 0 {
                    println!(
                        "[info] retrying command '{}' (attempt {}/{})",
                        cmd, attempt, max_retries
                    );
                    sleep(Duration::from_millis(500)).await;
                }

                match self.send_once(cmd).await {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        if attempt == max_retries {
                            return Err(e);
                        }
                    }
                }
            }
            unreachable!()
        }

        async fn send_once(&self, cmd: &str) -> anyhow::Result<()> {
            self.socket.send(cmd.as_bytes()).await?;
            let mut buf = [0u8; 128];

            // wait for response with timeout
            match tokio::time::timeout(Duration::from_secs(5), self.socket.recv(&mut buf)).await {
                Ok(Ok(n)) => {
                    let reply = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                    println!("[info] reply -> {}", reply);

                    // check for error response
                    if reply.contains("error") {
                        return Err(anyhow::anyhow!("Drone returned error: {}", reply));
                    }
                }
                Ok(Err(e)) => {
                    println!("[error] failed to receive response: {}", e);
                    return Err(anyhow::anyhow!("Failed to receive response: {}", e));
                }
                Err(_) => {
                    println!("[error] timeout waiting for response");
                    return Err(anyhow::anyhow!("Timeout waiting for response"));
                }
            }

            sleep(Duration::from_millis(20)).await;
            Ok(())
        }
    }

    impl DroneConnection for TelloDrone {
        async fn connect(&mut self) -> anyhow::Result<()> {
            println!(
                "[info] attempting to connect to tello at {}",
                self.tello_addr
            );

            // enter sdk mode with retry
            for attempt in 1..=3 {
                println!("[info] connection attempt {}/3", attempt);
                match self.send_with_retry("command", 2).await {
                    Ok(_) => {
                        println!(
                            "[info] connected! | q=takeoff, a=land, e/d/s/f move 20cm, esc=quit"
                        );
                        self.connected = true;
                        return Ok(());
                    }
                    Err(e) => {
                        println!("[error] attempt {} failed: {}", attempt, e);
                        if attempt < 3 {
                            sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            }

            println!("[error] failed to connect after 3 attempts");
            println!("[info] make sure:");
            println!("  1. tello is powered on");
            println!("  2. your computer is connected to tello's wifi network (TELLO-XXXXXX)");
            println!("  3. no other app is connected to the drone");
            Err(anyhow::anyhow!("Failed to establish connection"))
        }

        async fn send_command(&mut self, cmd: &str) -> anyhow::Result<()> {
            self.send_with_retry(cmd, 2).await?;
            println!("[info] cmd -> {}", cmd);
            Ok(())
        }

        async fn ensure_connected(&mut self) -> anyhow::Result<()> {
            if !self.connected {
                self.connect().await?;
            } else {
                // send keepalive
                self.send_with_retry("command", 1).await?;
            }
            Ok(())
        }

        async fn get_video_stream(&self) -> Option<VideoStream> {
            // placeholder for future video stream implementation
            None
        }
    }
}

mod controller {
    use super::*;

    pub enum Action {
        Command(String),
        NoOp,
        Exit,
    }

    pub trait Controller: Send + Sync {
        async fn get_action(&self) -> anyhow::Result<Option<Action>>;
    }

    pub struct ManualController;

    impl ManualController {
        pub fn new() -> Self {
            Self
        }
    }

    impl Controller for ManualController {
        async fn get_action(&self) -> anyhow::Result<Option<Action>> {
            if poll(Duration::from_millis(200))? {
                if let Event::Key(key) = read()? {
                    let action = match key.code {
                        KeyCode::Char('q') => Action::Command("takeoff".to_string()),
                        KeyCode::Char('a') => Action::Command("land".to_string()),
                        KeyCode::Char('e') => Action::Command("forward 20".to_string()),
                        KeyCode::Char('d') => Action::Command("back 20".to_string()),
                        KeyCode::Char('s') => Action::Command("left 20".to_string()),
                        KeyCode::Char('f') => Action::Command("right 20".to_string()),
                        KeyCode::Esc => Action::Exit,
                        _ => Action::NoOp,
                    };
                    return Ok(Some(action));
                }
            }
            Ok(None)
        }
    }

    // future implementation
    pub struct LLMController {
        // will contain:
        // - video stream reference
        // - llm client
        // - frame buffer
        // - decision making logic
    }
}
