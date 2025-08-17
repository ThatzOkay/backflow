mod serial_slider;

use std::time::{SystemTime, UNIX_EPOCH};
use crate::feedback::{FeedbackEvent, FeedbackEventStream, LedEvent};
use crate::input::{InputBackend, InputEvent, InputEventPacket, InputEventStream, KeyboardEvent};
use crate::input::io4_serial::serial_slider::{SerialSlider, SliderCommand};

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
        feedback_stream: FeedbackEventStream
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
            return Err(eyre::eyre!("Failed to initialize slider. Make sure it is connected"));
        }

        let hw_info = slider.get_hw_info();

        if hw_info.is_err() {
            tracing::error!("Failed to get slider hardware info: {:?}", hw_info);
            return Err(eyre::eyre!("Failed to get slider hardware info"));
        }

        // Send command to start touch input
        slider.send_command(SliderCommand::InputStart, None);

        let mut init_led_send = false;
        let mut error_count = 0;
        loop {
            if let Ok(Some(mut touch)) = slider.receive_input() {

                if !self.has_chuniio_proxy_config {
                    slider.send_led_reactive(touch);

                    touch.reverse();
                    let packet = input_state_tracker.diff_and_packet(&touch);

                    if packet.is_some() {
                        tracing::debug!("Sending slider input: {:?}", packet);
                        let result = self.input_stream.send(packet.unwrap()).await;

                        match result {
                            Ok(_) => {}
                            Err(e) => {
                                tracing::error!("Failed to send slider input: {:?}", e);
                                return Err(eyre::eyre!("Failed to send slider input"));
                            }
                        }
                    }
                    continue;
                }

                // Here chuniio proxy is enabled

                error_count = 0;

                if !init_led_send {
                    slider.send_led_reactive(touch);
                    init_led_send = true;
                }
            } else {
                error_count += 1;
                if error_count >= 2 {
                    slider.send_command(SliderCommand::InputStart, None);
                    init_led_send = false;
                    error_count = 0;
                }
            }
            let feed_back_packet = self.feedback_stream.receive().await;

            if let Some(feedback) = feed_back_packet {
                let led_packets = feedback.events.iter().filter(|packet| {
                    match packet {
                        FeedbackEvent::Led(led_event) => {
                            match led_event {
                                LedEvent::Set { led_id, on, brightness, rgb } => {
                                    true
                                }
                                _ => false
                            }
                        },
                        _ => false
                    }
                }).collect::<Vec<_>>();
                tracing::debug!("{}", led_packets.len());
            }

            // let packet = match touch_input {
            //     Ok(None) => continue,
            //     Ok(Some(input)) => input_state_tracker.diff_and_packet(&input),
            //     Err(e) => {
            //         tracing::error!("Failed to receive slider input: {:?}", e);
            //         return Err(eyre::eyre!("Failed to receive slider input"));
            //     }
            // };
            //
            // if packet.is_some() {
            //     tracing::debug!("Sending slider input: {:?}", packet);
            //     let result = self.input_stream.send(packet.unwrap()).await;
            //
            //     match result {
            //         Ok(_) => {}
            //         Err(e) => {
            //             tracing::error!("Failed to send slider input: {:?}", e);
            //             return Err(eyre::eyre!("Failed to send slider input"));
            //         }
            //     }
            // }
        }
    }
}


/// Tracks preview input state to emit only changed events
struct SliderSerialInputStateTracker {
    prev_slider: Vec<u8>
}

impl SliderSerialInputStateTracker {
    fn new() -> Self {
        Self {
            prev_slider: vec![0; 32]
        }
    }

    fn diff_and_packet(
        &mut self,
        slider: &[u8]
    ) -> Option<InputEventPacket> {
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