#!/usr/bin/env python3
"""Generate a slow SPI waveform on Raspberry Pi 5 GPIO lines.

Requires the Linux GPIO character-device Python bindings:
    sudo apt install python3-libgpiod
"""

import time

import gpiod

# Edit these constants for a different Pi wiring or test payload.
GPIO_CHIP = "/dev/gpiochip4"
CS_GPIO = 17
CLK_GPIO = 27
MOSI_GPIO = 22
CLOCK_HZ = 1_000
REPEAT = 100
TEXT = "Hello from Pi 5!\n"


class Outputs:
    """Small compatibility layer for libgpiod Python v1 and v2."""

    def __init__(self, chip_path, offsets):
        self.v2 = hasattr(gpiod, "request_lines")
        if self.v2:
            from gpiod.line import Direction, Value

            self.value = Value
            self.request = gpiod.request_lines(
                chip_path,
                consumer="u3pro16-spi-test",
                config={
                    offset: gpiod.LineSettings(direction=Direction.OUTPUT)
                    for offset in offsets
                },
            )
        else:
            self.chip = gpiod.Chip(chip_path)
            self.lines = {}
            for offset in offsets:
                line = self.chip.get_line(offset)
                line.request(
                    consumer="u3pro16-spi-test",
                    type=gpiod.LINE_REQ_DIR_OUT,
                    default_vals=[0],
                )
                self.lines[offset] = line

    def set(self, offset, value):
        if self.v2:
            self.request.set_value(
                offset,
                self.value.ACTIVE if value else self.value.INACTIVE,
            )
        else:
            self.lines[offset].set_value(value)

    def close(self):
        if self.v2:
            self.request.release()
        else:
            for line in self.lines.values():
                line.release()
            self.chip.close()


def main():
    payload = list(TEXT.encode("ascii"))
    gpio = Outputs(GPIO_CHIP, [CS_GPIO, CLK_GPIO, MOSI_GPIO])
    gpio.set(CS_GPIO, 1)
    gpio.set(CLK_GPIO, 0)
    gpio.set(MOSI_GPIO, 0)
    half_period = 0.5 / CLOCK_HZ

    try:
        for _ in range(REPEAT):
            gpio.set(CS_GPIO, 0)
            time.sleep(half_period)
            for byte in payload:
                for shift in range(7, -1, -1):
                    gpio.set(MOSI_GPIO, (byte >> shift) & 1)
                    time.sleep(half_period)
                    gpio.set(CLK_GPIO, 1)
                    time.sleep(half_period)
                    gpio.set(CLK_GPIO, 0)
            gpio.set(CS_GPIO, 1)
            gpio.set(MOSI_GPIO, 0)
            time.sleep(4 * half_period)
    finally:
        gpio.set(CS_GPIO, 1)
        gpio.set(CLK_GPIO, 0)
        gpio.set(MOSI_GPIO, 0)
        gpio.close()


if __name__ == "__main__":
    main()
