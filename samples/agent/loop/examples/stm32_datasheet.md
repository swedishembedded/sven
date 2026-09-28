# STM32F405 / STM32F407 — Technical Fact Sheet

**Manufacturer:** STMicroelectronics. **Source:** [DS8626 Rev 12, March 2026, 206 pages](https://www.st.com/resource/en/datasheet/dm00037051.pdf). **Scope:** STM32F405xx and STM32F407xx; capacities, exposed signals and features depend on the exact ordering code and package. Numerical maxima below are conditional ratings, not necessarily simultaneously achievable.

**Processor, buses and memory**

- **CPU:** 32-bit Arm Cortex-M4 with single-precision hardware FPU and DSP instructions; up to **168 MHz**, **210 DMIPS** / 1.25 DMIPS/MHz (Dhrystone 2.1). Cortex-M3 binary compatibility.
- **Flash execution:** ART accelerator with instruction prefetch and branch cache; 128-bit flash interface. ST describes benchmark performance equivalent to zero-wait-state execution; actual flash latency still requires configuration.
- **Protection/interrupts:** MPU with 8 regions, each divisible into 8 subregions; region sizes from 32 bytes. NVIC supports up to 82 peripheral interrupt channels and 16 priority levels. EXTI has 23 lines, including 16 GPIO interrupt lines.
- **Clock domains:** AHB up to 168 MHz; APB1 up to 42 MHz; APB2 up to 84 MHz. Internal regulator VOS=0 limits HCLK to 144 MHz; VOS=1 permits 168 MHz.
- **Nonvolatile memory:** 512 KiB or 1 MiB embedded flash, plus 512 bytes OTP. Density code **E = 512 KiB**, **G = 1 MiB**. Minimum specified flash endurance: 10,000 erase cycles. Retention: 30 years at 85°C after 1,000 cycles; 10 years at 105°C after 1,000 cycles; 20 years at 55°C after 10,000 cycles.
- **RAM:** 192 KiB total main RAM = 112 KiB SRAM1 + 16 KiB SRAM2 + 64 KiB core-coupled **data** RAM; additional 4 KiB backup SRAM. CCM connects directly to the CPU data path and is not a general DMA buffer.
- **DMA:** DMA1 and DMA2, 8 streams each; FIFOs, bursts, circular operation, double buffering and hardware peripheral requests. Multi-AHB bus matrix connects CPU, memories and bus masters.
- **External memory:** FSMC supports SRAM, PSRAM, NOR, NAND and CompactFlash/PCCard; synchronous FSMC clock up to 60 MHz. Parallel LCD controller interfaces support Intel 8080/Motorola 6800 modes. No FSMC on STM32F405RG; 100-pin/WLCSP implementations expose restricted banks/signals.

**Connectivity and conversion**

| Block | Capability and limits |
|---|---|
| USART/UART | 4 USARTs + 2 UARTs. USART1/6: up to 10.5 Mbit/s; USART2/3 and UART4/5: up to 5.25 Mbit/s with 8× oversampling. LIN, IrDA and single-wire half-duplex; USARTs additionally provide synchronous operation, RTS/CTS and ISO 7816 smartcard support. |
| SPI / I²S | 3 SPI controllers: SPI1 up to 42 Mbit/s; SPI2/3 up to 21 Mbit/s. 8-/16-bit SPI frames, hardware CRC and DMA. SPI2/3 alternatively provide 2 full-duplex I²S interfaces; 8–192 kHz audio sampling, dedicated PLLI2S or external clock. |
| I²C | 3 controllers; 100/400 kHz, multimaster/slave, 7-/10-bit addressing, dual slave addresses, SMBus 2.0/PMBus and DMA. |
| CAN | 2 bxCAN controllers; CAN 2.0A/B, up to 1 Mbit/s, 11-/29-bit identifiers. Per controller: 3 TX mailboxes and two 3-deep RX FIFOs; 28 shared filter banks. Classical CAN, not CAN FD. |
| SDIO | SD/SDIO/MMC host, clock up to 48 MHz; 1-/4-bit SD/SDIO and 1-/4-/8-bit MMC buses. SD 2.0, SDIO 2.0, MMC 4.2 and CE-ATA 1.1 support. |
| USB OTG FS | USB 2.0 device/host/OTG, integrated 12 Mbit/s PHY; 4 bidirectional endpoints, 8 host channels. |
| USB OTG HS | Separate device/host/OTG controller with dedicated DMA; 6 bidirectional endpoints, 12 host channels. Integrated PHY provides full speed; **480 Mbit/s requires an external ULPI PHY**. |
| Ethernet — F407 only | 10/100 Mbit/s MAC, dedicated DMA, MII/RMII, half/full duplex, VLAN and IEEE 1588v2 hardware timestamping. **External Ethernet PHY required.** |
| Camera — F407 only | DCMI parallel input, 8/10/12/14 bits; specified up to 54 MB/s at 54 MHz. Continuous/snapshot capture, cropping; raw Bayer, monochrome, RGB565, YCbCr 4:2:2 and compressed input such as JPEG. |
| ADC | 3 × 12-bit ADCs; up to 24 distinct external analog inputs across the device. Headline maxima: 2.4 MS/s per ADC, 7.2 MS/s triple-interleaved. At 30 MHz ADC clock and 3-cycle sampling, Table 67 specifies 2 MS/s single-ADC. Scan, simultaneous/interleaved sampling, timer triggers, analog watchdog and DMA. Internal temperature, reference and battery monitoring. |
| DAC | 2 buffered 12-bit channels; 8-/12-bit data modes, independent/synchronized updates, timer triggers, DMA, noise and triangle generation. |

**Timers, clocks, boot and debug**

- **Timers:** TIM1/TIM8 are 16-bit advanced motor-control timers with complementary PWM, dead-time and break input. TIM2/TIM5 are 32-bit; TIM3/4/9–14 are 16-bit general-purpose timers; TIM6/7 are basic 16-bit timers. Up to 4 capture/compare channels per timer; encoder modes on supported timers. Independent watchdog, window watchdog and 24-bit Cortex SysTick.
- **Clock sources:** 16 MHz internal HSI selected after reset; 4–26 MHz external crystal oscillator; main PLL; dedicated audio PLL; 32.768 kHz LSE and nominal 32 kHz LSI. Clock security can detect HSE failure and switch to HSI.
- **Boot:** user flash, system ROM or embedded SRAM. ROM bootloader supports USART1, USART3, CAN2 and USB OTG FS DFU on specified pins.
- **Support blocks:** calendar RTC, subsecond operation, 20 × 32-bit backup registers; 32-bit-output true RNG; CRC unit using a fixed polynomial; 96-bit unique device identifier. Debug through SWD/JTAG; Embedded Trace Macrocell.

**Electrical, power and packaging**

- **Supply:** VDD/VDDA normally 1.8–3.6 V; VBAT 1.65–3.6 V. VDDA must track VDD. The 1.7 V operating option requires restricted temperature conditions and an external supervisor with internal reset disabled on supported packages.
- **Power modes:** Sleep stops the CPU while peripherals operate; Stop retains SRAM/registers while high-speed clocks stop; Standby loses main SRAM/register state while the backup domain and optionally backup SRAM remain powered. POR/PDR, programmable BOR and PVD are provided.
- **Backup current example:** at 25°C and VBAT=3.3 V, typical current is 0.96 µA with low-speed oscillator/RTC enabled and backup SRAM disabled; 1.68 µA with backup SRAM enabled. These are backup-domain figures, not whole-chip running current.
- **Temperature:** suffix 6: −40 to +85°C ambient; suffix 7: −40 to +105°C ambient at specified dissipation. Junction limits are +105°C and +125°C respectively; extended ambient operation requires reduced dissipation.
- **GPIO:** up to 140; up to 138 are 5 V tolerant and up to 136 support high-speed operation. Maximum specified toggling rate is 84 MHz. Analog pins are not universally 5 V tolerant; alternate-function sharing limits concurrent interface availability.
- **Packages / maximum GPIO:** LQFP64, 10×10 mm / 51; WLCSP90, approximately 4.223×3.969 mm / 72; LQFP100, 14×14 mm / 82; LQFP144, 20×20 mm / 114; LQFP176, 24×24 mm or UFBGA176, 10×10 mm / 140. Availability depends on variant.
- **Family distinction:** STM32F405 lacks Ethernet and DCMI; STM32F407 includes both. Package selection determines accessible ADC channels, bus widths and peripheral pin combinations.

*Source map: features/variants pp. 1–2, 15–16; architecture/peripherals pp. 21–42; electrical limits pp. 81–83; backup current p. 93; flash endurance p. 112; ADC specifications pp. 135–138; packages/ordering pp. 165–186.*
