#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{AnyPin, Input, Level, Output, Pull, Speed};
use embassy_stm32::peripherals::{PE3, PE4};
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use {defmt_rtt as _, panic_probe as _};

static SHARED: Channel<ThreadModeRawMutex, Msg, 2> = Channel::new();

enum Msg {
    Increase,
    Decrease,
}

//#[embassy_executor::task]
async fn button1_task(mut pin: ExtiInput<'_, PE4>) {
    loop {
        pin.wait_for_rising_edge().await;
        SHARED.send(Msg::Increase).await;
        info!("BUTTON 1");

        Timer::after(Duration::from_millis(500)).await;
    }
}

//#[embassy_executor::task]
async fn button2_task(mut pin: ExtiInput<'_, PE3>) {
    loop {
        pin.wait_for_rising_edge().await;
        SHARED.send(Msg::Decrease).await;
        info!("BUTTON 2");

        Timer::after(Duration::from_millis(500)).await;
    }
}
//#[embassy_executor::task]
async fn blinker(led: AnyPin) {
    let mut led = Output::new(led, Level::High, Speed::Low);
    let mut freq = 1000;

    loop {
        let msg = SHARED.try_receive();
        if let Ok(message) = msg {
            match message {
                Msg::Increase => freq += 200,
                Msg::Decrease => {
                    if freq > 1000 {
                        freq -= 200;
                    } else {
                        freq = 1000;
                    }
                }
                _ => {}
            }
        }

        led.toggle();
        Timer::after(Duration::from_millis(freq)).await;
    }
}
#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_stm32::init(Default::default());
    info!("Hello World!");
    let mut button1 = Input::new(p.PE4, Pull::Up);
    let mut button1 = ExtiInput::new(button1, p.EXTI4);

    let mut button2 = Input::new(p.PE3, Pull::Up);
    let mut button2 = ExtiInput::new(button2, p.EXTI3);
    let mut led1 = p.PA6;

    //spawner.spawn(blinker(led1.into())).unwrap();
    //spawner.spawn(button1_task(button1)).unwrap();
    //spawner.spawn(button2_task(button2)).unwrap();

    futures::join!(blinker(led1.into()), button1_task(button1), button2_task(button2));
    loop {
        info!("Waiting for button press.");
        Timer::after(Duration::from_millis(1000));
    }
}
