mod serial_slider;


use crate::feedback::{FeedbackEvent, FeedbackEventStream, LedEvent};
use crate::input::io4_serial::serial_slider::{SerialSlider, SliderCommand};
use crate::input::{InputBackend, InputEvent, InputEventPacket, InputEventStream, KeyboardEvent};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct SliderSerialBackend {
    pub has_chuniio_proxy_config: bool,
    pub serial_port: String,
    pub input_stream: InputEventStream,
    pub feedback_stream: FeedbackEventStream,
}

impl SliderSerialBackend {
    pub fn new(
        has_chuniio_proxy_config: bool,
        serial_port: String,
        input_stream: InputEventStream,
        feedback_stream: FeedbackEventStream,
    ) -> Self {
        Self {
            has_chuniio_proxy_config,
            serial_port,
            input_stream,
            feedback_stream,
        }
    }
}

#[async_trait::async_trait]
impl InputBackend for SliderSerialBackend {
    async fn run(&mut self) -> eyre::Result<()> {
        let mut input_state_tracker = SliderSerialInputStateTracker::new();
        let slider = SerialSlider::new(self.serial_port.clone());

        if slider.is_err() {
            tracing::error!("Failed to talk to slider");
            return Err(eyre::eyre!("Failed to talk to slider"));
        }

        let mut slider = slider.unwrap();

        let init_result = slider.init_slider();

        if init_result.is_err() || init_result? == false {
            tracing::error!("Failed to initialize slider. Make sure it is connected");
            return Err(eyre::eyre!(
                "Failed to initialize slider. Make sure it is connected"
            ));
        }

        tracing::info!("Slider initialized");

        let hw_info = slider.get_hw_info();

        if hw_info.is_err() {
            tracing::error!("Failed to get slider hardware info: {:?}", hw_info);
            return Err(eyre::eyre!("Failed to get slider hardware info"));
        }

        tracing::info!("Slider hardware info: {:?}", hw_info?);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Send command to start touch input
        slider.send_command(SliderCommand::InputStart, None);

        slider.send_led_reactive([0; 32]);

        let mut error_count = 0;

        let shared_slider = Arc::new(tokio::sync::Mutex::new(slider));
        let reader_slider = Arc::clone(&shared_slider);
        let writer_slider = Arc::clone(&shared_slider);

        let has_chuniio_proxy_config = self.has_chuniio_proxy_config.clone();
        let feedback_stream = self.feedback_stream.clone();
        let input_stream = self.input_stream.clone();

        let writer = tokio::spawn(async move {
            loop {
                if !has_chuniio_proxy_config {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                let feedback_packet = feedback_stream.receive().await;
                if let Some(feedback) = feedback_packet {
                    let mut slider = writer_slider.lock().await;
                    let led_packets = feedback
                        .events
                        .iter()
                        .filter(|packet| match packet {
                            FeedbackEvent::Led(led_event) => match led_event {
                                LedEvent::Set {
                                    led_id: _,
                                    on: _,
                                    brightness: _,
                                    rgb: _,
                                } => true,
                                _ => false,
                            },
                            _ => false,
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    slider.send_led(led_packets);
                    drop(slider);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        });

        let reader: tokio::task::JoinHandle<eyre::Result<()>> = tokio::spawn(async move {
            loop {
                let mut slider = reader_slider.lock().await;
                if let Ok(Some(mut touch)) = slider.receive_input() {
                    if !has_chuniio_proxy_config {
                        slider.send_led_reactive(touch.clone());

                        touch.reverse();
                        let packet = input_state_tracker.diff_and_packet(&touch);

                        if let Some(packet) = packet {
                            let result = input_stream.send(packet).await;
                            match result {
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::error!("Failed to send slider input: {:?}", e);
                                    return Err(eyre::eyre!("Failed to send slider input"));
                                }
                            }
                        }
                        drop(slider);
                    } else {
                        // Here chuniio proxy is enabled
                        touch.reverse();
                        let packet = input_state_tracker.diff_and_packet(&touch);

                        if let Some(packet) = packet {
                            let result = input_stream.send(packet).await;
                            match result {
                                Ok(_) => {}
                                Err(e) => {
                                    tracing::error!("Failed to send slider input: {:?}", e);
                                    return Err(eyre::eyre!("Failed to send slider input"));
                                }
                            }
                        }

                        drop(slider);
                    }
                } else {
                    error_count += 1;
                    if error_count >= 2 {
                        slider.send_command(SliderCommand::InputStart, None);
                        // slider.send_led_reactive([
                        //     0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                        //     0, 0, 0, 0, 0, 0, 0, 0,
                        // ]);
                        error_count = 0;
                    }

                    drop(slider);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        });

        tracing::info!("Starting slider serial backend");
        let _ = tokio::join!(reader, writer);

        Ok(())
    }
}

/// Tracks preview input state to emit only changed events
struct SliderSerialInputStateTracker {
    prev_slider: Vec<u8>,
}

impl SliderSerialInputStateTracker {
    fn new() -> Self {
        Self {
            prev_slider: vec![0; 32],
        }
    }

    fn diff_and_packet(&mut self, slider: &[u8]) -> Option<InputEventPacket> {
        let mut events = Vec::new();

        // Slider - direct comparison without debouncing
        for (i, (&prev, &curr)) in self.prev_slider.iter().zip(slider.iter()).enumerate() {
            let key = format!("CHUNIIO_SLIDER_{i}");
            if prev < 128 && curr >= 128 {
                events.push(InputEvent::Keyboard(KeyboardEvent::KeyPress {
                    key: key.clone(),
                }));
            } else if prev >= 128 && curr < 128 {
                events.push(InputEvent::Keyboard(KeyboardEvent::KeyRelease {
                    key: key.clone(),
                }));
            }
        }

        // Update state - use raw slider values directly
        self.prev_slider.copy_from_slice(slider);
        if events.is_empty() {
            None
        } else {
            let device_id = "io4_serial".to_string();
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            Some(InputEventPacket {
                device_id,
                timestamp,
                events,
            })
        }
    }
}
