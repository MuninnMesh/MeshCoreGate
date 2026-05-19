//! Blocking SPI wrapper for an SX126x radio.

pub mod err;

use core::convert::Infallible;

use embedded_hal::digital::{ErrorType, InputPin, OutputPin};
use embedded_hal::spi::{Operation, SpiDevice};
use err::SpiError;

use self::err::{PinError, SxError};
use crate::conf::Config;
use crate::op::*;
use crate::reg::*;

/// Pin bundle accepted by [`SX126x::new`].
type Pins<TNRST, TBUSY, TANT, TDIO1> = (TNRST, TBUSY, TANT, TDIO1);

/// Pin bundle accepted by [`SX126x::new_without_ant`].
type PinsWithoutAntenna<TNRST, TBUSY, TDIO1> = (TNRST, TBUSY, TDIO1);

/// No-op byte used as the SX126x SPI dummy byte.
const NOP: u8 = 0x00;

/// Maximum BUSY pin polls before returning a timeout.
const BUSY_WAIT_POLLS: u32 = 500_000;

/// Maximum DIO1 pin polls before returning a timeout.
const DIO1_WAIT_POLLS: u32 = 500_000;

/// Calculates the rf_freq value that should be passed to SX126x::set_rf_frequency
/// based on the desired RF frequency and the XTAL frequency.
///
/// Example calculation for 868MHz:
/// 13.4.1.: RFfrequecy = (RFfreq * Fxtal) / 2^25 = 868M
/// -> RFfreq =
/// -> RFfrequecy ~ ((RFfreq >> 12) * (Fxtal >> 12)) >> 1
pub fn calc_rf_freq(rf_frequency: f32, f_xtal: f32) -> u32
{
    (rf_frequency * (33554432. / f_xtal)) as u32
}

/// No-op antenna control pin for boards without a separate antenna-enable GPIO.
pub struct NoAntenna;

impl ErrorType for NoAntenna
{
    type Error = Infallible;
}

impl OutputPin for NoAntenna
{
    fn set_low(&mut self) -> Result<(), Self::Error>
    {
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error>
    {
        Ok(())
    }
}

/// Wrapper around a Semtech SX1261/62 LoRa modem.
pub struct SX126x<TSPI: SpiDevice, TNRST, TBUSY, TANT, TDIO1>
{
    /// SPI device connected to the radio.
    spi:      TSPI,
    /// Active-low reset pin.
    nrst_pin: TNRST,
    /// BUSY input pin.
    busy_pin: TBUSY,
    /// Board antenna-enable output pin.
    ant_pin:  TANT,
    /// DIO1 IRQ input pin.
    dio1_pin: TDIO1,
}

impl<TSPI, TNRST, TBUSY, TDIO1> SX126x<TSPI, TNRST, TBUSY, NoAntenna, TDIO1>
where
    TSPI: SpiDevice,
{
    /// Create a driver without a separate antenna-enable pin.
    pub fn new_without_ant(spi: TSPI, pins: PinsWithoutAntenna<TNRST, TBUSY, TDIO1>) -> Self
    {
        let (nrst_pin, busy_pin, dio1_pin) = pins;
        Self {
            spi,
            nrst_pin,
            busy_pin,
            ant_pin: NoAntenna,
            dio1_pin,
        }
    }
}

impl<TSPI, TNRST, TBUSY, TANT, TDIO1, TSPIERR, TPINERR> SX126x<TSPI, TNRST, TBUSY, TANT, TDIO1>
where
    TPINERR: core::fmt::Debug,
    TSPI: SpiDevice<Error = TSPIERR>,
    TNRST: OutputPin<Error = TPINERR>,
    TBUSY: InputPin<Error = TPINERR>,
    TANT: OutputPin<Error = TPINERR>,
    TDIO1: InputPin<Error = TPINERR>,
{
    /// Create a driver from an SPI device and reset, busy, antenna, and DIO1 pins.
    pub fn new(spi: TSPI, pins: Pins<TNRST, TBUSY, TANT, TDIO1>) -> Self
    {
        let (nrst_pin, busy_pin, ant_pin, dio1_pin) = pins;
        Self {
            spi,
            nrst_pin,
            busy_pin,
            ant_pin,
            dio1_pin,
        }
    }

    /// Reset and configure the radio from a [`Config`].
    pub fn init(&mut self, conf: Config) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        // Reset the sx
        self.reset()?;
        self.wait_on_busy()?;

        // 1. If not in STDBY_RC mode, then go to this mode with the command SetStandby(...)
        self.set_standby(crate::op::StandbyConfig::StbyRc)?;
        self.wait_on_busy()?;

        // 2. Define the protocol (LoRa® or FSK) with the command SetPacketType(...)
        self.set_packet_type(conf.packet_type)?;
        self.wait_on_busy()?;

        // 3. Define the RF frequency with the command SetRfFrequency(...)
        self.set_rf_frequency(conf.rf_freq)?;
        self.wait_on_busy()?;

        if let Some((tcxo_voltage, tcxo_delay)) = conf.tcxo_opts {
            self.set_dio3_as_tcxo_ctrl(tcxo_voltage, tcxo_delay)?;
            self.wait_on_busy()?;
        }

        // Calibrate
        self.calibrate(conf.calib_param)?;
        self.wait_on_busy()?;
        // CalibrateImage is called separately in radio.rs with proper band bucketing

        // 4. Define the Power Amplifier configuration with the command SetPaConfig(...)
        self.set_pa_config(conf.pa_config)?;
        self.wait_on_busy()?;

        // 5. Define output power and ramping time with the command SetTxParams(...)
        self.set_tx_params(conf.tx_params)?;
        self.wait_on_busy()?;

        // 6. Define where the data payload will be stored with the command
        //    SetBufferBaseAddress(...)
        self.set_buffer_base_address(0x00, 0x00)?;
        self.wait_on_busy()?;

        // 7. Send the payload to the data buffer with the command WriteBuffer(...)
        // This is done later in SX126x::write_bytes

        // 8. Define the modulation parameter according to the chosen protocol with the command
        //    SetModulationParams(...) 1
        self.set_mod_params(conf.mod_params)?;
        self.wait_on_busy()?;

        // 9. Define the frame format to be used with the command SetPacketParams(...) 2
        if let Some(packet_params) = conf.packet_params {
            self.set_packet_params(packet_params)?;
            self.wait_on_busy()?;
        }

        // 10. Configure DIO and IRQ: use the command SetDioIrqParams(...) to select TxDone IRQ and
        //     map this IRQ to a DIO (DIO1,
        // DIO2 or DIO3)
        let irq_mask = conf
            .dio1_irq_mask
            .union(conf.dio2_irq_mask)
            .union(conf.dio3_irq_mask);
        self.set_dio_irq_params(
            irq_mask,
            conf.dio1_irq_mask,
            conf.dio2_irq_mask,
            conf.dio3_irq_mask,
        )?;
        self.wait_on_busy()?;
        if conf.dio2_rf_switch_ctrl {
            self.set_dio2_as_rf_switch_ctrl(true)?;
            self.wait_on_busy()?;
        }

        // 11. Optionally define the LoRa sync word via direct register access.
        if let Some(sync_word) = conf.sync_word {
            self.set_sync_word(sync_word)?;
            self.wait_on_busy()?;
        }

        // The rest of the steps are done by the user
        Ok(())
    }

    /// Set the LoRa sync word registers.
    pub fn set_sync_word(&mut self, sync_word: u16) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.write_register(Register::LoRaSyncWordMsb, &sync_word.to_be_bytes())
    }

    /// Set the modem packet type, which can be either GFSK of LoRa
    /// Note: GFSK is not fully supported by this crate at the moment
    pub fn set_packet_type(
        &mut self,
        packet_type: PacketType,
    ) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        self.spi
            .write(&[0x8A, packet_type as u8])
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Put the modem in standby mode
    pub fn set_standby(
        &mut self,

        standby_config: StandbyConfig,
    ) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        self.spi
            .write(&[0x80, standby_config as u8])
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Select regulator mode: 0x00 = LDO only, 0x01 = DC-DC + LDO.
    /// DC-DC mode reduces RX current draw by ~50%.
    pub fn set_regulator_mode_dcdc(&mut self) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        self.spi
            .write(&[0x96, 0x01])
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Get the current status of the modem
    pub fn get_status(&mut self) -> Result<Status, SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let mut result = [0xC0, NOP];
        self.spi
            .transfer_in_place(&mut result)
            .map_err(SpiError::Transfer)?;

        Ok(result[1].into())
    }

    /// Get instantaneous RSSI (in dBm).
    pub fn get_rssi_inst(&mut self) -> Result<f32, SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let mut result = [NOP, NOP];
        let mut ops = [Operation::Write(&[0x15, NOP]), Operation::Read(&mut result)];
        self.spi.transaction(&mut ops).map_err(SpiError::Transfer)?;
        Ok((result[1] as f32) / -2.0)
    }

    /// Put the radio in frequency-synthesis mode.
    pub fn set_fs(&mut self) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        self.spi.write(&[0xC1]).map_err(SpiError::Write)?;
        Ok(())
    }

    /// Return packet statistics accumulated by the radio.
    pub fn get_stats(&mut self) -> Result<Stats, SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let mut result = [0x10, NOP, NOP, NOP, NOP, NOP, NOP, NOP];
        self.spi
            .transfer_in_place(&mut result)
            .map_err(SpiError::Transfer)?;

        Ok([
            result[1], result[2], result[3], result[4], result[5], result[6], result[7],
        ]
        .into())
    }

    /// Calibrate image
    pub fn calibrate_image(&mut self, freq: CalibImageFreq)
    -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let freq: [u8; 2] = freq.into();
        let mut ops = [Operation::Write(&[0x98]), Operation::Write(&freq)];
        self.spi
            .transaction(&mut ops)
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Calibrate modem
    pub fn calibrate(&mut self, calib_param: CalibParam) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        self.spi
            .write(&[0x89, calib_param.into()])
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Write data to a register by raw address
    pub fn write_register_raw(
        &mut self,
        addr: u16,
        data: &[u8],
    ) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let start_addr = addr.to_be_bytes();
        let mut ops = [
            Operation::Write(&[0x0D]),
            Operation::Write(&start_addr),
            Operation::Write(data),
        ];
        self.spi.transaction(&mut ops).map_err(SpiError::Write)?;
        Ok(())
    }

    /// Write data into a register
    pub fn write_register(
        &mut self,

        register: Register,
        data: &[u8],
    ) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let start_addr = (register as u16).to_be_bytes();
        let mut ops = [
            Operation::Write(&[0x0D]),
            Operation::Write(&start_addr),
            Operation::Write(data),
        ];

        self.spi.transaction(&mut ops).map_err(SpiError::Write)?;
        Ok(())
    }

    /// Read data from a register.
    /// Internal implementation handles the mandatory dummy byte after the address.
    pub fn read_register(
        &mut self,

        start_addr: u16,
        result: &mut [u8],
    ) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        debug_assert!(!result.is_empty());
        let start_addr = start_addr.to_be_bytes();

        let mut ops = [
            Operation::Write(&[0x1D]),
            Operation::Write(&start_addr),
            Operation::Write(&[NOP]), // Mandatory dummy byte
            Operation::Read(result),
        ];

        self.spi.transaction(&mut ops).map_err(SpiError::Transfer)?;
        Ok(())
    }

    /// Put the modem in sleep mode.
    /// warm_start: if true, configuration is retained.
    pub fn set_sleep(&mut self, warm_start: bool) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let sleep_config = if warm_start { 0x04 } else { 0x00 };
        self.spi
            .write(&[0x84, sleep_config])
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Set Over Current Protection (OCP) configuration.
    /// For SX1262 +22dBm, use 0x38 (140mA).
    pub fn set_ocp_configuration(&mut self, ocp_ma: u8) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        // Formula: OCP(mA) = steps * 2.5mA. 0x38 = 56 decimal. 56 * 2.5 = 140mA.
        self.write_register(Register::OcpConfiguration, &[ocp_ma])
    }

    /// Write data into the buffer at the defined offset
    pub fn write_buffer(&mut self, offset: u8, data: &[u8])
    -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let header = [0x0E, offset];
        let mut ops = [Operation::Write(&header), Operation::Write(data)];
        self.spi
            .transaction(&mut ops)
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Read data from the data from the defined offset
    pub fn read_buffer(
        &mut self,

        offset: u8,
        result: &mut [u8],
    ) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let header = [0x1E, offset, NOP];
        let mut ops = [Operation::Write(&header), Operation::Read(result)];
        self.spi
            .transaction(&mut ops)
            .map_err(SpiError::Transfer)
            .map_err(Into::into)
    }

    /// Configure the dio2 pin as RF control switch
    pub fn set_dio2_as_rf_switch_ctrl(
        &mut self,

        enable: bool,
    ) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        self.spi
            .write(&[0x9D, enable as u8])
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Return status for the most recently received packet.
    pub fn get_packet_status(&mut self) -> Result<PacketStatus, SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let header = [0x14, NOP];
        let mut result = [NOP; 3];
        let mut ops = [Operation::Write(&header), Operation::Read(&mut result)];
        self.spi.transaction(&mut ops).map_err(SpiError::Transfer)?;

        Ok(result.into())
    }

    /// Configure the dio3 pin as TCXO control switch
    pub fn set_dio3_as_tcxo_ctrl(
        &mut self,

        tcxo_voltage: TcxoVoltage,
        tcxo_delay: TcxoDelay,
    ) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let header = [0x97, tcxo_voltage as u8];
        let tcxo_delay: [u8; 3] = tcxo_delay.into();
        let mut ops = [Operation::Write(&header), Operation::Write(&tcxo_delay)];
        self.spi
            .transaction(&mut ops)
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Clear device error register
    pub fn clear_device_errors(&mut self) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        self.spi
            .write(&[0x07, NOP, NOP])
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Get current device errors
    pub fn get_device_errors(&mut self) -> Result<DeviceErrors, SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let mut result = [0x17, NOP, NOP, NOP];
        self.spi
            .transfer_in_place(&mut result)
            .map_err(SpiError::Transfer)?;
        Ok(DeviceErrors::from(u16::from_be_bytes([
            result[2], result[3],
        ])))
    }

    /// Reset the device py pulling nrst low for a while
    pub fn reset(&mut self) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        critical_section::with(|_| {
            self.nrst_pin.set_low().map_err(PinError::Output)?;
            // 8.1: The pin should be held low for typically 100 μs for the Reset to happen
            self.spi
                .transaction(&mut [Operation::DelayNs(200_000)])
                .map_err(SpiError::Write)?;
            self.nrst_pin
                .set_high()
                .map_err(PinError::Output)
                .map_err(Into::into)
        })
    }

    /// Enable antenna
    pub fn set_ant_enabled(&mut self, enabled: bool) -> Result<(), TPINERR>
    {
        if enabled {
            self.ant_pin.set_high()
        } else {
            self.ant_pin.set_low()
        }
    }

    /// Configure IRQ
    pub fn set_dio_irq_params(
        &mut self,

        irq_mask: IrqMask,
        dio1_mask: IrqMask,
        dio2_mask: IrqMask,
        dio3_mask: IrqMask,
    ) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let irq = (Into::<u16>::into(irq_mask)).to_be_bytes();
        let dio1 = (Into::<u16>::into(dio1_mask)).to_be_bytes();
        let dio2 = (Into::<u16>::into(dio2_mask)).to_be_bytes();
        let dio3 = (Into::<u16>::into(dio3_mask)).to_be_bytes();
        let mut ops = [
            Operation::Write(&[0x08]),
            Operation::Write(&irq),
            Operation::Write(&dio1),
            Operation::Write(&dio2),
            Operation::Write(&dio3),
        ];
        self.spi
            .transaction(&mut ops)
            .map_err(SpiError::Transfer)
            .map_err(Into::into)
    }

    /// Get the current IRQ status
    pub fn get_irq_status(&mut self) -> Result<IrqStatus, SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let mut status = [NOP, NOP, NOP];
        let mut ops = [Operation::Write(&[0x12]), Operation::Read(&mut status)];
        self.spi.transaction(&mut ops).map_err(SpiError::Transfer)?;
        let irq_status: [u8; 2] = [status[1], status[2]];
        Ok(u16::from_be_bytes(irq_status).into())
    }

    /// Clear the IRQ status
    pub fn clear_irq_status(&mut self, mask: IrqMask) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let mask = Into::<u16>::into(mask).to_be_bytes();
        let mut ops = [Operation::Write(&[0x02]), Operation::Write(&mask)];
        self.spi
            .transaction(&mut ops)
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Put the device in TX mode. It will start sending the data written in the buffer,
    /// starting at the configured offset
    pub fn set_tx(&mut self, timeout: RxTxTimeout) -> Result<Status, SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let mut buf = [0x83u8; 4];
        let timeout: [u8; 3] = timeout.into();
        buf[1..].copy_from_slice(&timeout);

        self.spi
            .transfer_in_place(&mut buf)
            .map_err(SpiError::Transfer)?;
        Ok(buf[0].into())
    }

    /// Put the radio in RX mode with the given timeout.
    pub fn set_rx(&mut self, timeout: RxTxTimeout) -> Result<Status, SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let mut buf = [0x82u8; 4];
        let timeout: [u8; 3] = timeout.into();
        buf[1..].copy_from_slice(&timeout);

        self.spi
            .transfer_in_place(&mut buf)
            .map_err(SpiError::Transfer)?;
        Ok(buf[0].into())
    }

    /// Set packet parameters (LoRa: 6 bytes, no trailing padding)
    pub fn set_packet_params(
        &mut self,

        params: PacketParams,
    ) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let mut ops = [
            Operation::Write(&[0x8C]),
            Operation::Write(params.as_bytes()),
        ];
        self.spi
            .transaction(&mut ops)
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Set modulation parameters (LoRa: 4 bytes, no trailing padding)
    pub fn set_mod_params(&mut self, params: ModParams) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let mut ops = [
            Operation::Write(&[0x8B]),
            Operation::Write(params.as_bytes()),
        ];
        self.spi
            .transaction(&mut ops)
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Set TX parameters
    pub fn set_tx_params(&mut self, params: TxParams) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let params: [u8; 2] = params.into();
        let mut ops = [Operation::Write(&[0x8E]), Operation::Write(&params)];
        self.spi
            .transaction(&mut ops)
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Set RF frequency. This writes the passed rf_freq directly to the modem.
    /// Use sx1262::calc_rf_freq to calulate the correct value based
    /// On the XTAL frequency and the desired RF frequency
    pub fn set_rf_frequency(&mut self, rf_freq: u32) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let rf_freq = rf_freq.to_be_bytes();
        let mut ops = [Operation::Write(&[0x86]), Operation::Write(&rf_freq)];
        self.spi
            .transaction(&mut ops)
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Set Power Amplifier configuration
    pub fn set_pa_config(&mut self, pa_config: PaConfig) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let pa_config: [u8; 4] = pa_config.into();
        let mut ops = [Operation::Write(&[0x95]), Operation::Write(&pa_config)];
        self.spi
            .transaction(&mut ops)
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// Configure the base addresses in the buffer
    pub fn set_buffer_base_address(
        &mut self,

        tx_base_addr: u8,
        rx_base_addr: u8,
    ) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        self.spi
            .write(&[0x8F, tx_base_addr, rx_base_addr])
            .map_err(SpiError::Write)
            .map_err(Into::into)
    }

    /// High level method to send a message. This methods writes the data in the buffer,
    /// puts the device in TX mode, and waits until the devices
    /// is done sending the data or a timeout occurs.
    /// Please note that this method updates the packet params
    pub fn write_bytes(
        &mut self,
        data: &[u8],
        timeout: RxTxTimeout,
        preamble_len: u16,
        crc_type: packet::LoRaCrcType,
    ) -> Result<Status, SxError<TSPIERR, TPINERR>>
    {
        use packet::LoRaPacketParams;
        if data.len() > u8::MAX as usize {
            return Err(SxError::InvalidPayloadLength);
        }
        let payload_len = data.len() as u8;

        // Write data to buffer
        self.write_buffer(0x00, data)?;

        // Set packet params
        let params = LoRaPacketParams::default()
            .set_preamble_len(preamble_len)
            .set_payload_len(payload_len)
            .set_crc_type(crc_type)
            .into();

        self.set_packet_params(params)?;

        // Set tx mode
        let status = self.set_tx(timeout)?;
        // Wait for busy line to go low
        self.wait_on_busy()?;
        // Wait on dio1 going high
        self.wait_on_dio1()?;
        // Clear IRQ
        self.clear_irq_status(IrqMask::all())?;
        // Write completed!
        Ok(status)
    }

    /// Get Rx buffer status, containing the length of the last received packet
    /// and the address of the first byte received.
    pub fn get_rx_buffer_status(&mut self) -> Result<RxBufferStatus, SxError<TSPIERR, TPINERR>>
    {
        self.wait_on_busy()?;
        let mut result = [0x13, NOP, NOP, NOP];
        self.spi
            .transfer_in_place(&mut result)
            .map_err(SpiError::Transfer)?;
        Ok([result[2], result[3]].into())
    }

    /// Busily wait for the busy pin to go low.
    /// Times out after ~50ms to prevent infinite hangs if the SX1262 gets stuck
    /// (e.g. after voltage droop during TX corrupts chip state).
    /// Normal operations clear BUSY in <1ms; TCXO warmup + calibration can take ~25ms.
    pub fn wait_on_busy(&mut self) -> Result<(), SxError<TSPIERR, TPINERR>>
    {
        self.spi
            .transaction(&mut [Operation::DelayNs(1000)])
            .map_err(SpiError::Transfer)?;
        // At 80 MHz, each GPIO read + branch ≈ 100-200ns. 500_000 iterations ≈ 50-100ms.
        // This covers TCXO warmup (20ms) + calibration (3.5ms) + CalibrateImage (8ms)
        // with generous margin, while still preventing infinite hangs.
        for _ in 0..BUSY_WAIT_POLLS {
            match self.busy_pin.is_high() {
                Ok(true) => {},
                Ok(false) => return Ok(()),
                Err(err) => return Err(PinError::Input(err).into()),
            }
        }
        Err(SpiError::BusyTimeout.into())
    }

    /// Check the BUSY pin and report GPIO read failures.
    pub fn try_is_busy(&mut self) -> Result<bool, PinError<TPINERR>>
    {
        self.busy_pin.is_high().map_err(PinError::Input)
    }

    /// Check whether the radio is busy, returning `false` on GPIO read failure.
    pub fn is_busy(&mut self) -> bool
    {
        self.try_is_busy().unwrap_or(false)
    }

    /// Busily wait for the dio1 pin to go high
    fn wait_on_dio1(&mut self) -> Result<(), PinError<TPINERR>>
    {
        for _ in 0..DIO1_WAIT_POLLS {
            match self.dio1_pin.is_low() {
                Ok(true) => {},
                Ok(false) => return Ok(()),
                Err(err) => return Err(PinError::Input(err)),
            }
        }
        Err(PinError::Dio1Timeout)
    }
}
