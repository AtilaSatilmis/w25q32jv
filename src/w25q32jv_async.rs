use super::*;
use core::fmt::Debug;
use embedded_hal::digital::OutputPin;
use embedded_hal_async::spi::{Operation, SpiDevice};
use embedded_storage_async::nor_flash::{MultiwriteNorFlash, NorFlash, ReadNorFlash};

impl<SPI, S: Debug, P: Debug, HOLD, WP> ReadNorFlash for W25q32jv<SPI, HOLD, WP>
where
    SPI: SpiDevice<Error = S>,
    HOLD: OutputPin<Error = P>,
    WP: OutputPin<Error = P>,
    S: Debug,
    P: Debug,
{
    const READ_SIZE: usize = 1;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.read_async(offset, bytes).await
    }

    fn capacity(&self) -> usize {
        Self::capacity()
    }
}

impl<SPI, S: Debug, P: Debug, HOLD, WP> NorFlash for W25q32jv<SPI, HOLD, WP>
where
    SPI: SpiDevice<Error = S>,
    HOLD: OutputPin<Error = P>,
    WP: OutputPin<Error = P>,
    S: Debug,
    P: Debug,
{
    const WRITE_SIZE: usize = 1;

    const ERASE_SIZE: usize = SECTOR_SIZE as usize;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.erase_range_async(from, to).await
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.write_async(offset, bytes).await
    }
}

impl<SPI, S: Debug, P: Debug, HOLD, WP> MultiwriteNorFlash for W25q32jv<SPI, HOLD, WP>
where
    SPI: SpiDevice<Error = S>,
    HOLD: OutputPin<Error = P>,
    WP: OutputPin<Error = P>,
    S: Debug,
    P: Debug,
{
}

impl<SPI, S: Debug, P: Debug, HOLD, WP> W25q32jv<SPI, HOLD, WP>
where
    SPI: SpiDevice<Error = S>,
    HOLD: OutputPin<Error = P>,
    WP: OutputPin<Error = P>,
    S: Debug,
    P: Debug,
{
    async fn read_status_register_async(&mut self) -> Result<u8, Error<S, P>> {
        let mut buf: [u8; 2] = [0; 2];
        buf[0] = Command::ReadStatusRegister1 as u8;

        self.spi
            .transfer_in_place(&mut buf)
            .await
            .map_err(Error::SpiError)?;

        Ok(buf[1])
    }

    /// The flash chip is unable to perform new commands while it is still working on a previous one. Especially erases take a long time.
    /// This function returns true while the chip is unable to respond to commands (with the exception of the busy command).
    async fn busy_async(&mut self) -> Result<bool, Error<S, P>> {
        Ok((self.read_status_register_async().await? & 0x01) != 0)
    }

    async fn write_enabled_async(&mut self) -> Result<bool, Error<S, P>> {
        Ok((self.read_status_register_async().await? & 0x02) != 0)
    }

    /// Request the 64 bit id that is unique to this chip.
    pub async fn device_id_async(&mut self) -> Result<[u8; 8], Error<S, P>> {
        let mut buf: [u8; 13] = [0; 13];
        buf[0] = Command::UniqueId as u8;

        self.spi
            .transfer_in_place(&mut buf)
            .await
            .map_err(Error::SpiError)?;

        Ok(TryFrom::try_from(&buf[5..]).unwrap())
    }

    /// Reset the chip
    pub async fn reset_async(&mut self) -> Result<(), Error<S, P>> {
        self.spi
            .write(&[Command::EnableReset as u8])
            .await
            .map_err(Error::SpiError)?;
        self.spi
            .write(&[Command::Reset as u8])
            .await
            .map_err(Error::SpiError)?;
        Ok(())
    }

    /// Reads a chunk of bytes from the flash chip.
    /// The number of bytes read is equal to the length of the buf slice.
    /// The first byte is read from the provided address. This address is then incremented for each following byte.
    ///
    /// # Arguments
    /// * `address` - Address where the first byte of the buf will be read.
    /// * `buf` - Slice that is going to be filled with the read bytes.
    pub async fn read_async(&mut self, address: u32, buf: &mut [u8]) -> Result<(), Error<S, P>> {
        if address + buf.len() as u32 > CAPACITY {
            return Err(Error::OutOfBounds);
        }

        self.spi
            .transaction(&mut [
                Operation::Write(&command_and_address(Command::ReadData as u8, address)),
                Operation::Read(buf),
            ])
            .await
            .map_err(Error::SpiError)?;

        Ok(())
    }

    /// Sets the enable_write flag on the flash chip to true.
    /// Writes and erases to the chip only have effect when this flag is true.
    /// Each write and erase clears the flag, requiring it to be set to true again for the next command.
    async fn enable_write_async(&mut self) -> Result<(), Error<S, P>> {
        self.spi
            .write(&[Command::WriteEnable as u8])
            .await
            .map_err(Error::SpiError)?;

        if !self.write_enabled_async().await? {
            return Err(Error::WriteEnableFail);
        }

        Ok(())
    }

    /// Writes a chunk of bytes to the flash chip.
    /// The first byte is written to the provided address. This address is then incremented for each following byte.
    ///
    /// # Arguments
    /// * `address` - Address where the first byte of the buf will be written.
    /// * `buf` - Slice of bytes that will be written.
    pub async fn write_async(
        &mut self,
        mut address: u32,
        mut buf: &[u8],
    ) -> Result<(), Error<S, P>> {
        if address + buf.len() as u32 > CAPACITY {
            return Err(Error::OutOfBounds);
        }

        // Write first chunk, taking into account that given addres might
        // point to a location that is not on a page boundary,
        let chunk_len = (PAGE_SIZE - (address & 0x000000FF)) as usize;
        let chunk_len = chunk_len.min(buf.len());
        self.write_page_async(address, &buf[..chunk_len]).await?;

        // Write rest of the chunks
        let mut chunk_len = chunk_len;
        loop {
            buf = &buf[chunk_len..];
            address += chunk_len as u32;
            chunk_len = buf.len().min(PAGE_SIZE as usize);
            if chunk_len == 0 {
                break;
            }
            self.write_page_async(address, &buf[..chunk_len]).await?;
        }

        Ok(())
    }

    /// Execute a write on a single page
    async fn write_page_async(&mut self, address: u32, buf: &[u8]) -> Result<(), Error<S, P>> {
        // We don't support wrapping writes. They're scary
        if (address & 0x000000FF) + buf.len() as u32 > PAGE_SIZE {
            return Err(Error::OutOfBounds);
        }

        self.enable_write_async().await?;

        self.spi
            .transaction(&mut [
                Operation::Write(&command_and_address(Command::PageProgram as u8, address)),
                Operation::Write(buf),
            ])
            .await
            .map_err(Error::SpiError)?;

        while self.busy_async().await? {}

        if cfg!(feature = "readback-check") {
            self.readback_check_async(address, buf).await?;
        }

        Ok(())
    }

    async fn readback_check_async(
        &mut self,
        mut address: u32,
        data: &[u8],
    ) -> Result<(), Error<S, P>> {
        const CHUNK_SIZE: usize = 64;

        let mut buf = [0; CHUNK_SIZE];

        for chunk in data.chunks(CHUNK_SIZE) {
            let buf = &mut buf[..chunk.len()];
            self.read_async(address, buf).await?;
            address += CHUNK_SIZE as u32;

            if buf != chunk {
                return Err(Error::ReadbackFail);
            }
        }

        Ok(())
    }

    /// Erases a range of sectors. The range is expressed in bytes. These bytes need to be a multiple of SECTOR_SIZE.
    /// If the range starts at SECTOR_SIZE * 3 then the erase starts at the fourth sector.
    /// All sectors are erased in the range [start_sector..end_sector].
    /// The start address may not be a higher value than the end address.
    ///
    /// # Arguments
    /// * `start_address` - Address of the first byte of the start of the range of sectors that need to be erased.
    /// * `end_address` - Address of the first byte of the end of the range of sectors that need to be erased.
    pub async fn erase_range_async(
        &mut self,
        start_address: u32,
        end_address: u32,
    ) -> Result<(), Error<S, P>> {
        if start_address % (SECTOR_SIZE) != 0 {
            return Err(Error::NotAligned);
        }

        if end_address % (SECTOR_SIZE) != 0 {
            return Err(Error::NotAligned);
        }

        if start_address > end_address {
            return Err(Error::OutOfBounds);
        }

        let start_sector = start_address / SECTOR_SIZE;
        let end_sector = end_address / SECTOR_SIZE;

        for sector in start_sector..end_sector {
            self.erase_sector_async(sector).await.unwrap();
        }

        Ok(())
    }

    /// Erases a single sector of flash memory with the size of SECTOR_SIZE.
    ///
    /// # Arguments
    /// * `index` - the index of the sector that needs to be erased. The address of the first byte of the sector is the provided index * SECTOR_SIZE.
    pub async fn erase_sector_async(&mut self, index: u32) -> Result<(), Error<S, P>> {
        if index >= N_SECTORS {
            return Err(Error::OutOfBounds);
        }

        self.enable_write_async().await?;

        let address: u32 = index * SECTOR_SIZE;

        self.spi
            .write(&command_and_address(Command::SectorErase as u8, address))
            .await
            .map_err(Error::SpiError)?;

        while self.busy_async().await? {}

        if cfg!(feature = "readback-check") {
            for offset in (0..SECTOR_SIZE).step_by(64) {
                self.readback_check_async(address + offset, &[0xFF; 64])
                    .await?;
            }
        }

        Ok(())
    }

    /// Erases a single block of flash memory with the size of BLOCK_32K_SIZE.
    ///
    /// # Arguments
    /// * `index` - the index of the block that needs to be erased. The address of the first byte of the block is the provided index * BLOCK_32K_SIZE.
    pub async fn erase_block_32k_async(&mut self, index: u32) -> Result<(), Error<S, P>> {
        if index >= N_BLOCKS_32K {
            return Err(Error::OutOfBounds);
        }

        self.enable_write_async().await?;

        let address: u32 = index * BLOCK_32K_SIZE;

        self.spi
            .write(&command_and_address(Command::Block32Erase as u8, address))
            .await
            .map_err(Error::SpiError)?;

        while self.busy_async().await? {}

        if cfg!(feature = "readback-check") {
            for offset in (0..BLOCK_32K_SIZE).step_by(64) {
                self.readback_check_async(address + offset, &[0xFF; 64])
                    .await?;
            }
        }

        Ok(())
    }

    /// Erases a single block of flash memory with the size of BLOCK_64K_SIZE.
    ///
    /// # Arguments
    /// * `index` - the index of the block that needs to be erased. The address of the first byte of the block is the provided index * BLOCK_64K_SIZE.
    pub async fn erase_block_64k_async(&mut self, index: u32) -> Result<(), Error<S, P>> {
        if index >= N_BLOCKS_64K {
            return Err(Error::OutOfBounds);
        }

        self.enable_write_async().await?;

        let address: u32 = index * BLOCK_64K_SIZE;

        self.spi
            .write(&command_and_address(Command::Block64Erase as u8, address))
            .await
            .map_err(Error::SpiError)?;

        while self.busy_async().await? {}

        if cfg!(feature = "readback-check") {
            for offset in (0..BLOCK_64K_SIZE).step_by(64) {
                self.readback_check_async(address + offset, &[0xFF; 64])
                    .await?;
            }
        }

        Ok(())
    }

    /// Erases all sectors on the flash chip.
    /// This is a very expensive operation.
    pub async fn erase_chip_async(&mut self) -> Result<(), Error<S, P>> {
        self.enable_write_async().await?;

        self.spi
            .write(&[Command::ChipErase as u8])
            .await
            .map_err(Error::SpiError)?;

        while self.busy_async().await? {}

        if cfg!(feature = "readback-check") {
            for address in (0..CAPACITY).step_by(64) {
                self.readback_check_async(address, &[0xFF; 64]).await?;
            }
        }

        Ok(())
    }

    /// Puts the chip into power down mode.
    /// While in the power-down state, only the Release Power-down/Device ID (0xAB) instruction will be recognized. This instruction restores the device to normal operation. All other instructions are ignored.
    pub async fn enable_power_down_mode_async(&mut self) -> Result<(), Error<S, P>> {
        self.spi
            .write(&[Command::PowerDown as u8])
            .await
            .map_err(Error::SpiError)?;

        Ok(())
    }

    /// Releases the chip from power down mode.
    /// Restores operation from power down mode by reading the deviceID from the device.
    pub async fn disable_power_down_mode_async(&mut self) -> Result<(), Error<S, P>> {
        self.spi
            .write(&[Command::ReleasePowerDown as u8])
            .await
            .map_err(Error::SpiError)?;

        Ok(())
    }
}
