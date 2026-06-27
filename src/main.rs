#![no_std]
#![no_main]

use core::sync::atomic::{
    AtomicU8, AtomicU32,
    Ordering::{Relaxed, SeqCst},
};

use chrono::{NaiveTime, TimeDelta, Timelike};
use embassy_executor::Spawner;
use embassy_rp::gpio::{AnyPin, Level, Output, Pin};
use embassy_time::{Duration, Timer};
use {defmt_rtt as _, panic_probe as _};

/// We have 8 LEDs per position so we represent the pattern of which LEDs to turn on for each character
/// by the bitstring of the underlying byte.
/// The LED output pins are saved in an array and the bit position corresponds to the array entry that is turned on.
/// [ll, lm, lr, mm, ul, um, ur, dot]
///  0   1   2   3   4   5   6   7
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LEDChar(u8);

impl LEDChar {
    const D0: Self = Self(0b01110111);
    const D1: Self = Self(0b01000100);
    const D2: Self = Self(0b01101011);
    const D3: Self = Self(0b01101110);
    const D4: Self = Self(0b01011100);
    const D5: Self = Self(0b00111110);
    const D6: Self = Self(0b00111111);
    const D7: Self = Self(0b01100100);
    const D8: Self = Self(0b01111111);
    const D9: Self = Self(0b01111110);
    const CA: Self = Self(0b11111101);
    const CP: Self = Self(0b11111001);
    const CH: Self = Self(0b11011101);
    const ERROR: Self = Self(0b10101010);
}

impl LEDChar {
    fn from_decimal(val: u32) -> Self {
        match val {
            0 => Self::D0,
            1 => Self::D1,
            2 => Self::D2,
            3 => Self::D3,
            4 => Self::D4,
            5 => Self::D5,
            6 => Self::D6,
            7 => Self::D7,
            8 => Self::D8,
            9 => Self::D9,
            _ => Self::ERROR,
        }
    }
}

static LED0: AtomicU32 = AtomicU32::new(0);
static LED1: AtomicU8 = AtomicU8::new(0);

struct LEDStateC<const N: usize> {
    chars: [LEDChar; N],
}

struct LEDState {
    led0: LEDStateC<4>,
    led1: LEDStateC<1>,
}

impl<const N: usize> LEDStateC<N> {
    fn to_bytes(self) -> [u8; N] {
        self.chars.map(|c| c.0)
    }

    fn from_bytes(bytes: [u8; N]) -> Self {
        Self {
            chars: bytes.map(LEDChar),
        }
    }
}

impl LEDState {
    fn write(self) {
        let state0 = u32::from_le_bytes(self.led0.to_bytes());
        let state1 = u8::from_le_bytes(self.led1.to_bytes());

        LED0.store(state0, SeqCst);
        LED1.store(state1, SeqCst);
    }

    fn read() -> Self {
        let state0 = LED0.load(SeqCst);
        let state1 = LED1.load(SeqCst);

        Self {
            led0: LEDStateC::from_bytes(u32::to_le_bytes(state0)),
            led1: LEDStateC::from_bytes(u8::to_le_bytes(state1)),
        }
    }

    fn from_naive_time_12h(time: NaiveTime) -> Self {
        let (am_pm, hh) = time.hour12();
        let mm = time.minute();

        let h10 = LEDChar::from_decimal(hh / 10);
        let h1 = LEDChar::from_decimal(hh % 10);
        let m10 = LEDChar::from_decimal(mm / 10);
        let m1 = LEDChar::from_decimal(mm % 10);
        let info = if am_pm { LEDChar::CP } else { LEDChar::CA };

        Self {
            led0: LEDStateC {
                chars: [h10, h1, m10, m1],
            },
            led1: LEDStateC { chars: [info] },
        }
    }
    fn from_naive_time_24h(time: NaiveTime) -> Self {
        let hh = time.hour();
        let mm = time.minute();

        let h10 = LEDChar::from_decimal(hh / 10);
        let h1 = LEDChar::from_decimal(hh % 10);
        let m10 = LEDChar::from_decimal(mm / 10);
        let m1 = LEDChar::from_decimal(mm % 10);
        let info = LEDChar::CH;

        Self {
            led0: LEDStateC {
                chars: [h10, h1, m10, m1],
            },
            led1: LEDStateC { chars: [info] },
        }
    }
}

struct LEDResources<'a> {
    leds: [Output<'a>; 8],
    outputs: [Output<'a>; 5],
}

impl<'a> LEDResources<'a> {
    async fn show(&mut self, outidx: usize, c: LEDChar) {
        let mut bits: u8 = c.0;

        self.outputs[outidx].set_low();
        for idx in 0..8 {
            if bits & 1 != 0 {
                self.leds[idx].set_high();
            } else {
                self.leds[idx].set_low();
            }
            bits = bits >> 1;
        }
        // Small delay so that the LEDs can actually light up.
        Timer::after_millis(1).await;
        self.outputs[outidx].set_high();
    }
}

#[embassy_executor::task]
async fn led_manager(mut res: LEDResources<'static>) {
    loop {
        let led_state = LEDState::read();

        for (outidx, c) in led_state
            .led0
            .chars
            .into_iter()
            .chain(led_state.led1.chars.into_iter())
            .enumerate()
        {
            res.show(outidx, c).await;
        }
    }
}

#[embassy_executor::task]
async fn timer(start: NaiveTime) {
    const ONE_MINUTE: TimeDelta = TimeDelta::try_minutes(1).expect("ONE_MINUTE malformed.");
    let mut current_time = start;

    loop {
        LEDState::from_naive_time_24h(current_time).write();
        Timer::after_secs(60).await;
        current_time = current_time.overflowing_add_signed(ONE_MINUTE).0;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) -> ! {
    let p = embassy_rp::init(Default::default());

    /* The Pin to LED mapping for each of the digit blocks is:
     *      15
     *    ┌────┐
     *    │    │
     *  14│    │13
     *    ├────┤
     *    │ 12 │
     *   8│    │11 ┌┐
     *    └────┘   └┘
     *      9      10
     * */
    let ll = Output::new(p.PIN_8, Level::Low);
    let lm = Output::new(p.PIN_9, Level::Low);
    let dot = Output::new(p.PIN_10, Level::Low);
    let lr = Output::new(p.PIN_11, Level::Low);
    let mm = Output::new(p.PIN_12, Level::Low);
    let ur = Output::new(p.PIN_13, Level::Low);
    let ul = Output::new(p.PIN_14, Level::Low);
    let um = Output::new(p.PIN_15, Level::Low);

    let leds: [Output; 8] = [ll, lm, lr, mm, ul, um, ur, dot];

    let outputs = [
        Output::new(p.PIN_21, Level::High), // X000 0
        Output::new(p.PIN_20, Level::High), // 0X00 0
        Output::new(p.PIN_19, Level::High), // 00X0 0
        Output::new(p.PIN_18, Level::High), // 000X 0
        Output::new(p.PIN_17, Level::High), // 0000 X
    ];

    const START_TIME: NaiveTime =
        NaiveTime::from_hms_opt(14, 37, 00).expect("START_TIME malformed.");
    spawner.spawn(timer(START_TIME).expect("Spawning time manager failed."));
    // Wait until global time is set.
    Timer::after_millis(1).await;
    spawner
        .spawn(led_manager(LEDResources { leds, outputs }).expect("Spawning led manager failed."));

    loop {
        Timer::after_millis(500).await
    }
}
