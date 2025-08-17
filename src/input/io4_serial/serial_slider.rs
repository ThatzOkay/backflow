use std::io::Read;
use serialport::SerialPort;
use std::time::Duration;

#[derive(Debug)]
pub struct SerialSlider {
    serial_port: Box<dyn SerialPort>,
}

#[derive(Clone, Debug)]
pub enum SliderCommand {
    SliderInit = 0x10,
    HwInfo = 0xf0,
    LedSet = 0x02,
    InputStart = 0x03,
}

impl SerialSlider {
    pub fn new(serial_port_name: String) -> Result<Self, Option<serialport::Error>> {
        let open_port_result = serialport::new(&serial_port_name, 115_200)
            .timeout(Duration::from_millis(10))
            .open();

        if open_port_result.is_err() {
            return Err(open_port_result.err())
        }

        let open_port = open_port_result?;

        Ok(Self {
            serial_port: open_port,
        })
    }

    fn create_command(command: SliderCommand, data: Option<&[u8]>) -> Vec<u8> {
        let mut buffer = Vec::new();

        buffer.push(0xff);

        let cmd = command.clone() as u8;
        buffer.push(cmd);

        match command {
            SliderCommand::LedSet => {
                buffer.push(0x5e);
                buffer.push(0x7f);
            }
            _ => {
                buffer.push(0x00);
            }
        }

        if let Some(extra) = data {
            buffer.extend_from_slice(extra);
        }

        buffer
    }

    fn read_byte(&mut self) -> Option<u8> {
        let mut buffer = [0u8; 1];
        match self.serial_port.read_exact(&mut buffer) {
            Ok(_) => Some(buffer[0]),
            Err(_) => None,
        }
    }

    pub fn checksum(&self, arr: &[u8]) -> u8 {
        let sum: u16 = arr.iter().map(|x| *x as u16).sum();
        let mut checksum = (!sum & 0xFF) + 1;
        checksum &= 0xFF;
        checksum as u8
    }

    pub fn send_command(&mut self, command: SliderCommand, data: Option<&[u8]>) {
        let mut command = Self::create_command(command, data);

        let checksum = self.checksum(&*command.clone());

        command.push(checksum);

        self.serial_port.write(command.as_slice()).unwrap();
    }

    pub fn init_slider(&mut self) -> Result<bool, std::io::Error> {
        let mut counter = 0;

        loop {
            self.send_command(SliderCommand::SliderInit, None);

            let mut response_buffer = [0u8; 4];
            let result = self.serial_port.read_exact(&mut response_buffer);

            if response_buffer == [0xff, 0x10, 0x00, 0xf1]  {
                return Ok(true);
            }

            if counter >= 3 {
                return Ok(false);
            }

            counter += 1;
        }
    }

    pub fn get_hw_info(&mut self) -> Result<String, std::io::Error> {
        let mut counter = 0;

        loop {
            self.send_command(SliderCommand::HwInfo, None);

            let mut response_buffer = [0u8; 22];
            let result = self.serial_port.read_exact(&mut response_buffer);

            if result.is_err() {
                return Err(result.err().unwrap());
            }

            if response_buffer
                == [
                0xff, 0xf0, 0x12, b'1', b'5', b'3', b'3', b'0', b' ', b' ', b' ', 0xa0, b'0',
                b'6', b'7', b'1', b'2', 0xfd, 0xfe, 0x90, 0x00, b'd',
            ]
                || counter >= 3
            {
                return Ok(String::from_utf8_lossy(&response_buffer).to_string());
            }

            counter += 1;
        }
    }

    pub fn receive_input(&mut self) -> Result<Option<[u8; 32]>, std::io::Error> {
        let mut response_buffer = [0u8; 32];

        let mut byte = match self.read_byte() {
            Some(b) => b,
            None => {
                return Ok(None)
            },
        };

        while byte != 0xff {
            byte = match self.read_byte() {
                Some(b) => b,
                None => {
                    return Ok(None)
                },
            };
        }

        loop {
            let next_byte = match self.read_byte() {
                Some(b) => b,
                None => {
                    tracing::error!("Failed to read byte next byte");
                    return Ok(None)
                },
            };
            if next_byte != 0xFF {
                byte = next_byte;
                break;
            }
        }

        if byte != 0x01 {
            tracing::error!("Unexpected command type: 0x{:02X}", byte);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "Unexpected command type"));
        }

        let packet_length = match self.read_byte() {
            Some(b) => b,
            None => {
                tracing::error!("Failed to read packet length");
                return Ok(None)
            },
        };

        if packet_length != 0x20 {
            tracing::error!("Unexpected packet length: 0x{:02X}", packet_length);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "Unexpected packet length"));
        }

        for i in 0..32 {
            response_buffer[i] = match self.read_byte() {
                Some(b) => b,
                None => return Ok(None),
            };
        }

        let _ = self.read_byte();
        Ok(Some(response_buffer))
    }

    pub fn send_led_reactive(&mut self, input_data: [u8; 32]) {
        let mut data = vec![];

        for x in 0..31 {
            if x & 1 == 1 {
                data.push(0x7f);
                data.push(0x23);
                data.push(0);
            }
            else if input_data[x] != 0 {
                data.push(0x23);
                data.push(0);
                data.push(0x7f);
            }
            else if input_data[x+1] != 0  {
                data.push(0);
                data.push(0x7f);
                data.push(0x23);
            }
            else {
                data.push(0);
                data.push(0);
                data.push(0);
            }
        }

        self.send_command(SliderCommand::LedSet, Some(&*data));
    }
}
