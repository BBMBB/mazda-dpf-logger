use std::error::Error;

#[cfg(feature = "pi-leds")]
use rppal::gpio::{Gpio, OutputPin};

pub enum LedState {
    Recording,
    Problem,
    Off,
}

pub struct StatusLeds {
    #[cfg(feature = "pi-leds")]
    pins: Option<(OutputPin, OutputPin)>,
}

impl StatusLeds {
    pub fn new(enabled: bool) -> Result<Self, Box<dyn Error>> {
        #[cfg(feature = "pi-leds")]
        {
            let pins = if enabled {
                let gpio = Gpio::new()?;
                // BCM numbering: GPIO17 = physical pin 11, GPIO27 = pin 13.
                let green = gpio.get(17)?.into_output_low();
                let red = gpio.get(27)?.into_output_high();
                Some((green, red))
            } else {
                None
            };
            Ok(Self { pins })
        }
        #[cfg(not(feature = "pi-leds"))]
        {
            if enabled {
                return Err("LED support is not compiled in; build with --features pi-leds".into());
            }
            Ok(Self {})
        }
    }

    pub fn set(&mut self, state: LedState) {
        #[cfg(feature = "pi-leds")]
        if let Some((green, red)) = &mut self.pins {
            // Turn the previous indication off before enabling the next one.
            green.set_low();
            red.set_low();
            match state {
                LedState::Recording => green.set_high(),
                LedState::Problem => red.set_high(),
                LedState::Off => {}
            }
        }
        #[cfg(not(feature = "pi-leds"))]
        let _ = state;
    }

    pub fn finish(&mut self, failed: bool) {
        self.set(if failed {
            LedState::Problem
        } else {
            LedState::Off
        });
        #[cfg(feature = "pi-leds")]
        if failed {
            if let Some((green, red)) = &mut self.pins {
                // Keep the failure visible after a fatal I/O error exits the process.
                green.set_reset_on_drop(false);
                red.set_reset_on_drop(false);
            }
        }
    }
}
