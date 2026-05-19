//! ESP32-S3 platform initialization and platform service hooks.

use alloc::boxed::Box;

use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::peripherals::{
    ADC1,
    GPIO0,
    GPIO1,
    GPIO2,
    GPIO7,
    GPIO8,
    GPIO9,
    GPIO10,
    GPIO11,
    GPIO12,
    GPIO13,
    GPIO14,
    GPIO15,
    GPIO17,
    GPIO18,
    GPIO21,
    GPIO36,
    GPIO37,
    GPIO46,
    I2C0,
    SPI2,
    USB_DEVICE,
    WIFI,
};
use esp_hal::rng::Rng;
use esp_hal::system::{AppCoreGuard, CpuControl, Stack};
use esp_hal::time::Instant;
use esp_hal::timer::timg::{Timer, TimerGroup};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use esp_wifi::EspWifiController;
use muninn_gate_core::Clock;

/// Stack reserved for the dedicated LoRa/MeshCore APP CPU task.
pub const RADIO_MESHCORE_STACK_BYTES: usize = 32 * 1024;
/// Internal allocator used by firmware code.
pub const INTERNAL_HEAP_BYTES: usize = 72 * 1024;
/// Settle time after enabling the board external peripheral rail.
pub const EXTERNAL_POWER_SETTLE_MS: u32 = 200;

static mut RADIO_MESHCORE_STACK: Stack<RADIO_MESHCORE_STACK_BYTES> = Stack::new();

/// ESP HAL resources consumed by the WiFi station runtime.
pub struct Esp32WifiResources
{
    /// Initialized WiFi scheduler/controller context.
    init:   Option<&'static EspWifiController<'static>>,
    /// Timer used by the WiFi scheduler before initialization.
    timer0: Option<Timer<'static>>,
    /// Hardware RNG used by the WiFi driver before initialization.
    rng:    Option<Rng>,
    /// ESP32 WiFi peripheral.
    wifi:   Option<WIFI<'static>>,
}

impl Esp32WifiResources
{
    /// Return initialized WiFi context and peripheral resources.
    pub fn into_initialized_parts(
        mut self,
    ) -> Option<(&'static EspWifiController<'static>, WIFI<'static>)>
    {
        Some((self.init.take()?, self.wifi.take()?))
    }

    /// Initialize WiFi if needed and return the context plus peripheral.
    pub fn initialize(
        mut self,
    ) -> Result<(&'static EspWifiController<'static>, WIFI<'static>), Esp32WifiInitError>
    {
        if let Some(init) = self.init.take() {
            let wifi = self
                .wifi
                .take()
                .ok_or(Esp32WifiInitError::ResourcesUnavailable)?;
            return Ok((init, wifi));
        }

        let timer0 = self
            .timer0
            .take()
            .ok_or(Esp32WifiInitError::ResourcesUnavailable)?;
        let rng = self
            .rng
            .take()
            .ok_or(Esp32WifiInitError::ResourcesUnavailable)?;
        let wifi = self
            .wifi
            .take()
            .ok_or(Esp32WifiInitError::ResourcesUnavailable)?;
        let init = esp_wifi::init(timer0, rng).map_err(|_| Esp32WifiInitError::DriverInit)?;
        let init = Box::leak(Box::new(init));

        Ok((init, wifi))
    }
}

/// ESP32-S3 resources wired to the board radio.
pub struct Esp32RadioResources
{
    /// SPI bus connected to the radio.
    pub spi2:           SPI2<'static>,
    /// Radio SPI clock pin.
    pub sck:            GPIO9<'static>,
    /// Radio SPI MOSI pin.
    pub mosi:           GPIO10<'static>,
    /// Radio SPI MISO pin.
    pub miso:           GPIO11<'static>,
    /// Radio chip-select pin.
    pub nss:            GPIO8<'static>,
    /// Radio reset pin.
    pub reset:          GPIO12<'static>,
    /// Radio busy pin.
    pub busy:           GPIO13<'static>,
    /// Radio DIO1 interrupt pin.
    pub dio1:           GPIO14<'static>,
    /// Antenna switch or RX-enable pin.
    pub ant:            GPIO15<'static>,
    /// Front-end module power pin.
    pub fem_power:      GPIO7<'static>,
    /// Front-end module enable pin.
    pub fem_en:         GPIO2<'static>,
    /// Front-end module PA-mode pin.
    pub fem_pa:         GPIO46<'static>,
    /// ADC unit used for board battery voltage measurement.
    pub battery_adc:    ADC1<'static>,
    /// Battery voltage divider sense pin.
    pub battery_pin:    GPIO1<'static>,
    /// Battery voltage divider enable pin.
    pub battery_enable: GPIO37<'static>,
}

/// ESP32-S3 resources wired to the local display.
pub struct Esp32DisplayResources
{
    /// I2C peripheral connected to the display bus.
    pub i2c0:  I2C0<'static>,
    /// I2C data pin.
    pub sda:   GPIO17<'static>,
    /// I2C clock pin.
    pub scl:   GPIO18<'static>,
    /// Display reset pin.
    pub reset: GPIO21<'static>,
}

/// ESP32-S3 resources wired to local user input controls.
pub struct Esp32InputResources
{
    /// User button pin.
    pub user_button: GPIO0<'static>,
}

/// Board-level resources not owned by WiFi or CPU scheduling.
pub struct Esp32BoardResources
{
    /// Radio wiring resources.
    pub radio:   Esp32RadioResources,
    /// Display wiring resources.
    pub display: Esp32DisplayResources,
    /// User input resources.
    pub input:   Esp32InputResources,
    /// External peripheral power rail control pin.
    pub vext:    GPIO36<'static>,
}

impl Esp32BoardResources
{
    /// Enable the external board rail and retain the power-control pin.
    pub fn enable_external_power(self) -> Esp32PoweredBoardResources
    {
        let mut vext = Output::new(self.vext, Level::Low, OutputConfig::default());
        vext.set_low();
        Delay::new().delay_millis(EXTERNAL_POWER_SETTLE_MS);

        Esp32PoweredBoardResources {
            radio:   self.radio,
            display: self.display,
            input:   self.input,
            power:   Esp32ExternalPowerGuard { _vext: vext },
        }
    }
}

/// Board resources after the external board rail has been enabled.
pub struct Esp32PoweredBoardResources
{
    /// Radio wiring resources.
    pub radio:   Esp32RadioResources,
    /// Display wiring resources.
    pub display: Esp32DisplayResources,
    /// User input resources.
    pub input:   Esp32InputResources,
    /// External power guard retained to keep the rail enabled.
    pub power:   Esp32ExternalPowerGuard,
}

/// Guard that keeps the external board rail enabled while owned.
pub struct Esp32ExternalPowerGuard
{
    /// Owned external-power output pin, retained to keep the rail enabled.
    _vext: Output<'static>,
}

/// ESP32-S3 CPU core used for a platform task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Esp32Core
{
    /// PRO CPU, used for startup, WiFi, HTTP, storage, and serial work.
    ProCpu,
    /// APP CPU, reserved for LoRa radio and MeshCore work.
    AppCpu,
}

/// ESP32-S3 platform task placement policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Esp32TaskPlan
{
    /// Core that owns the LoRa radio and MeshCore packet path.
    pub radio_meshcore_core: Esp32Core,
    /// Core that owns WiFi and HTTP serving.
    pub wifi_http_core:      Esp32Core,
    /// Core that owns storage and provisioning work.
    pub storage_core:        Esp32Core,
    /// Core that owns serial output and provisioning work.
    pub serial_core:         Esp32Core,
}

impl Esp32TaskPlan
{
    /// Return the default ESP32-S3 task plan for Muninn Gate.
    pub const fn default_esp32s3() -> Self
    {
        Self {
            radio_meshcore_core: Esp32Core::AppCpu,
            wifi_http_core:      Esp32Core::ProCpu,
            storage_core:        Esp32Core::ProCpu,
            serial_core:         Esp32Core::ProCpu,
        }
    }

    /// Return true when LoRa/MeshCore has an exclusive CPU assignment.
    pub const fn radio_meshcore_is_dedicated(&self) -> bool
    {
        matches!(self.radio_meshcore_core, Esp32Core::AppCpu)
            && matches!(self.wifi_http_core, Esp32Core::ProCpu)
            && matches!(self.storage_core, Esp32Core::ProCpu)
            && matches!(self.serial_core, Esp32Core::ProCpu)
    }
}

/// Errors returned while starting ESP32 platform tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Esp32TaskStartError
{
    /// The APP CPU could not be started or was already running.
    AppCoreUnavailable,
}

/// Errors returned while initializing ESP32 WiFi support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Esp32WifiInitError
{
    /// WiFi resources were already consumed or unavailable.
    ResourcesUnavailable,
    /// The ESP WiFi driver failed to initialize.
    DriverInit,
}

/// Initialized ESP32 platform handle.
pub struct Esp32Platform
{
    cpu_control:     CpuControl<'static>,
    app_core_guard:  Option<AppCoreGuard<'static>>,
    wifi_resources:  Option<Esp32WifiResources>,
    board_resources: Option<Esp32BoardResources>,
    usb_device:      Option<USB_DEVICE<'static>>,
}

/// Initialize ESP HAL and return platform services used by the gateway.
pub fn init() -> Esp32Platform
{
    esp_alloc::heap_allocator!(size: INTERNAL_HEAP_BYTES);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let cpu_control = CpuControl::new(peripherals.CPU_CTRL);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_hal_embassy::init(timg0.timer0);
    let timg1 = TimerGroup::new(peripherals.TIMG1);

    Esp32Platform {
        cpu_control,
        app_core_guard: None,
        wifi_resources: Some(Esp32WifiResources {
            init:   None,
            timer0: Some(timg1.timer0),
            rng:    Some(Rng::new(peripherals.RNG)),
            wifi:   Some(peripherals.WIFI),
        }),
        usb_device: Some(peripherals.USB_DEVICE),
        board_resources: Some(Esp32BoardResources {
            radio:   Esp32RadioResources {
                spi2:           peripherals.SPI2,
                sck:            peripherals.GPIO9,
                mosi:           peripherals.GPIO10,
                miso:           peripherals.GPIO11,
                nss:            peripherals.GPIO8,
                reset:          peripherals.GPIO12,
                busy:           peripherals.GPIO13,
                dio1:           peripherals.GPIO14,
                ant:            peripherals.GPIO15,
                fem_power:      peripherals.GPIO7,
                fem_en:         peripherals.GPIO2,
                fem_pa:         peripherals.GPIO46,
                battery_adc:    peripherals.ADC1,
                battery_pin:    peripherals.GPIO1,
                battery_enable: peripherals.GPIO37,
            },
            display: Esp32DisplayResources {
                i2c0:  peripherals.I2C0,
                sda:   peripherals.GPIO17,
                scl:   peripherals.GPIO18,
                reset: peripherals.GPIO21,
            },
            input:   Esp32InputResources {
                user_button: peripherals.GPIO0,
            },
            vext:    peripherals.GPIO36,
        }),
    }
}

impl Esp32Platform
{
    /// Return the task placement policy for this platform.
    pub const fn task_plan(&self) -> Esp32TaskPlan
    {
        Esp32TaskPlan::default_esp32s3()
    }

    /// Start the dedicated APP CPU task for LoRa and MeshCore ownership.
    pub fn start_radio_meshcore_core<F>(&mut self, entry: F) -> Result<(), Esp32TaskStartError>
    where
        F: FnOnce() + Send + 'static,
    {
        #[allow(static_mut_refs)]
        let stack = unsafe { &mut RADIO_MESHCORE_STACK };
        let guard = self
            .cpu_control
            .start_app_core(stack, entry)
            .map_err(|_| Esp32TaskStartError::AppCoreUnavailable)?;
        self.app_core_guard = Some(guard);
        Ok(())
    }

    /// Take WiFi resources for station initialization.
    pub fn take_wifi_resources(&mut self) -> Option<Esp32WifiResources>
    {
        self.wifi_resources.take()
    }

    /// Take board resources for radio and display initialization.
    pub fn take_board_resources(&mut self) -> Option<Esp32BoardResources>
    {
        self.board_resources.take()
    }

    /// Take USB Serial/JTAG for USB provisioning input.
    pub fn take_usb_serial(&mut self) -> Option<UsbSerialJtag<'static, Blocking>>
    {
        self.usb_device.take().map(UsbSerialJtag::new)
    }
}

impl Clock for Esp32Platform
{
    fn now_ms(&self) -> u64
    {
        Instant::now().duration_since_epoch().as_millis()
    }
}

/// Return platform-reported free heap bytes, when available.
pub fn free_heap_bytes() -> Option<u32>
{
    Some(esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into()) as u32)
}
