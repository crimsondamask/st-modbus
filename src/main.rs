#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use core::cell::RefCell;
use core::sync::atomic::AtomicU32;

use defmt::{panic, *};
use embassy_executor::Spawner;
use embassy_stm32::exti::ExtiInput;
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::peripherals::{PA6, PA7, PE3, PE4};
use embassy_stm32::time::mhz;
use embassy_stm32::usb_otg::{Driver, Instance};
use embassy_stm32::{bind_interrupts, peripherals, usb_otg, Config};
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::Builder;
use fixedvec::{alloc_stack, FixedVec};
use {defmt_rtt as _, panic_probe as _};

use rmodbus::{
    server::{context::ModbusContextSmall, ModbusFrame},
    ModbusProto,
};

// The channel that tasks use to communicate with each other.#########################

static SHARED2: Channel<ThreadModeRawMutex, Msg, 10> = Channel::new();

//#####################################################################################

// An Atomic u32 to store a testing value.#############################################

static SHARED_U32: AtomicU32 = AtomicU32::new(1000);

//#####################################################################################

// A mutex to store holding registers.#################################################

static HOLDING_REGS: Mutex<ThreadModeRawMutex, RefCell<[u16; 1000]>> =
    Mutex::new(RefCell::new([11; 1000]));

//#####################################################################################

bind_interrupts!(struct Irqs {
    OTG_FS => usb_otg::InterruptHandler<peripherals::USB_OTG_FS>;
});

// A Msg type enum that is used to send messages between tasks.
enum Msg {
    Increase,
    Decrease,
}

// Disconnected type for errors.
struct Disconnected {}

impl From<EndpointError> for Disconnected {
    fn from(val: EndpointError) -> Self {
        match val {
            EndpointError::BufferOverflow => panic!("Buffer overflow"),
            EndpointError::Disabled => Disconnected {},
        }
    }
}

// Async task #####################################################################
// The async task for handling Modbus communication over USB.
// It takes a Cdc class, reads bytes over USB into a buffer and
// constructs a Modbus frame from that buffer.
//#[embassy_executor::task]
async fn usb_handler_task<'d, T: Instance + 'd>(
    class: &mut CdcAcmClass<'d, Driver<'d, T>>,
) -> Result<(), Disconnected> {
    let mut buf = [0; 256];

    let mut modbus_context = ModbusContextSmall::new();
    loop {
        // reading raw packets from USB.
        let _n = class.read_packet(&mut buf).await?;

        // Allocating stack memory for the response buffer.
        let mut pre_alloc = alloc_stack!([u8; 256]);
        let mut response = FixedVec::new(&mut pre_alloc);

        // Constructing a Modbus frame from the read packets.
        let mut frame = ModbusFrame::new(1, &buf, ModbusProto::Rtu, &mut response);

        // Parsing the frame.
        if frame.parse().is_err() {
            info!("FRAME PARSING ERROR!");
            continue;
        }

        if frame.processing_required {
            // Checking for Read or Write commands.
            let result = match frame.readonly {
                true => {
                    // This is a little not very neat hack to update the holding registers.
                    // We update the modbus context which is local to this task by copying
                    // the values of the "globally" accessed HOLDING_REGS mutex.
                    modbus_context.holdings = HOLDING_REGS.lock(|f| f.clone().into_inner());
                    // We proceed to processing the commands.
                    frame.process_read(&modbus_context)
                }
                false => {
                    modbus_context.holdings = HOLDING_REGS.lock(|f| f.clone().into_inner());
                    frame.process_write(&mut modbus_context)
                }
            };

            if result.is_err() {
                info!("FRAME PROCESSING ERROR!");
                continue;
            }

            // We lock the HOLDING_REGS mutex and update it with the new register values
            // after processing Modbus commands. This makes sure that the mutex values
            // are in sync with the modbus_context values after register writes.
            HOLDING_REGS.lock(|f| {
                f.replace(modbus_context.holdings);
            });
        }

        if frame.response_required {
            frame.finalize_response().unwrap();
            // We finalize the Modbus response.
            info!("{:?}", response.as_slice());

            // We write the response as USB packets.
            if class.write_packet(response.as_slice()).await.is_ok() {
                info!("\ndata written!");
            };
        }
    }
}

// Async task #####################################################################
// This is for demonstration only. We wait for the button1 EXTI interupt and we send
// an Increase message over the shared channel to let other tasks know that the
// button has been pressed.
//#[embassy_executor::task]
async fn button1_task(mut pin: ExtiInput<'static, PE4>) {
    loop {
        pin.wait_for_rising_edge().await;
        SHARED2.send(Msg::Increase).await;

        // For demo we update the holding register 0 values.
        HOLDING_REGS.lock(|f| {
            f.borrow_mut()[0] += 100;
        });
        let current_value = SHARED_U32.load(core::sync::atomic::Ordering::Relaxed);
        // We insert some value into the Atomic.
        SHARED_U32.store(
            current_value.wrapping_add(100),
            core::sync::atomic::Ordering::Relaxed,
        );
        info!("BUTTON 1");
    }
}

// Async task #####################################################################
// Same as task one.
//#[embassy_executor::task]
async fn button2_task(mut pin: ExtiInput<'static, PE3>) {
    loop {
        pin.wait_for_rising_edge().await;
        SHARED2.send(Msg::Decrease).await;

        HOLDING_REGS.lock(|f| {
            f.borrow_mut()[0] = 0;
        });
        let current_value = SHARED_U32.load(core::sync::atomic::Ordering::Relaxed);

        if current_value > 100 {
            SHARED_U32.store(
                current_value.wrapping_sub(100),
                core::sync::atomic::Ordering::Relaxed,
            );
        }
        info!("BUTTON 2");
    }
}

// Async task #####################################################################
// A blinker task that keeps incrementing a register value. This is an example of
// monitoring a sensor value and storing it into a holding register.
//#[embassy_executor::task]
async fn blinker(mut led: Output<'static, PA6>) {
    loop {
        let duration = SHARED_U32.load(core::sync::atomic::Ordering::Relaxed);
        led.toggle();
        HOLDING_REGS.lock(|f| {
            f.borrow_mut()[1] += 1;
        });
        Timer::after(Duration::from_millis(duration as u64)).await;
    }
}

// Async task #####################################################################
//
//#[embassy_executor::task]
async fn blinker2(mut led: Output<'static, PA7>) {
    loop {
        if let Ok(msg) = SHARED2.try_receive() {
            match msg {
                Msg::Increase => {
                    for _ in 0..4 {
                        led.toggle();
                        Timer::after(Duration::from_millis(100)).await;
                    }
                }

                Msg::Decrease => {
                    for _ in 0..8 {
                        led.toggle();
                        Timer::after(Duration::from_millis(100)).await;
                    }
                }
            }
        }

        Timer::after(Duration::from_millis(50)).await;
    }
}

// Main executor task.
//
#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Configure the clocks.
    let mut config = Config::default();
    config.rcc.pll48 = true;
    config.rcc.sys_ck = Some(mhz(48));

    let p = embassy_stm32::init(config);
    info!("Hello World!");

    let mut ep_out_buffer = [0u8; 256];

    let mut config = embassy_stm32::usb_otg::Config::default();
    config.vbus_detection = true;
    let driver = Driver::new_fs(
        p.USB_OTG_FS,
        Irqs,
        p.PA12,
        p.PA11,
        &mut ep_out_buffer,
        config,
    );

    let mut config = embassy_usb::Config::new(0xc0de, 0xcafe);
    config.manufacturer = Some("Embassy");
    config.product = Some("USB-serial example");
    config.serial_number = Some("12345678");

    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;
    config.composite_with_iads = true;

    let mut device_descriptor = [0; 256];
    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    let mut control_buf = [0; 64];

    let mut state = State::new();

    let mut builder = Builder::new(
        driver,
        config,
        &mut device_descriptor,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut control_buf,
    );

    // Create classes on the builder.
    let mut class = CdcAcmClass::new(&mut builder, &mut state, 64);

    // Build the builder.
    let mut usb = builder.build();

    // Run the USB device.
    let usb_fut = usb.run();

    let button1 = Input::new(p.PE4, Pull::Up);
    let button1 = ExtiInput::new(button1, p.EXTI4);

    let button2 = Input::new(p.PE3, Pull::Up);
    let button2 = ExtiInput::new(button2, p.EXTI3);
    let led1 = p.PA6;
    let led1 = Output::new(led1, Level::Low, Speed::Low);

    let led2 = p.PA7;
    let led2 = Output::new(led2, Level::High, Speed::High);

    let echo_fut = async {
        loop {
            class.wait_connection().await;
            info!("Connected.");
            let _ = usb_handler_task(&mut class).await;
            info!("Disconnected");
        }
    };
    // Waiting on all the async tasks.
    futures::join!(
        blinker(led1.into()),
        blinker2(led2.into()),
        button1_task(button1),
        button2_task(button2),
        usb_fut,
        echo_fut,
    );
    loop {
        Timer::after(Duration::from_secs(1)).await;
        info!("Waiting for button press.");
    }
}
