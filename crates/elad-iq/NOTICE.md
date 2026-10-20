# Notice

`elad-iq` is a port of the ELAD USB driver from **SDRoxide**
(<https://github.com/dividebysandwich/sdroxide>), by dividebysandwich,
`crates/sdroxide-elad`, licensed GPL-3.0-or-later — the same licence as this
workspace. Each file copied from it names its SDRoxide source path in a header
comment. The changes for hardrock-panel are small: SDRoxide's shared types
(`EladConfig`, `EladDevice`, `ELAD_SAMPLE_RATES`, `Complex32`, …) are replaced
by local definitions in `src/types.rs`, user-facing messages name this program,
and `EladHandle::rx_read` copies in chunks.

SDRoxide's own source for the vendor protocol — device ids, control transfers,
tuning arithmetic, EEPROM calibration map and sample scaling — is ELAD's
**gr-elad** (<https://github.com/ELADIT/gr-elad>, `lib/fdm_source_c_impl.cc`,
GPL-3.0-or-later). The FPGA loading for the FDM-S1/S2 follows ELAD's Linux
loader and SoapyELAD (<https://github.com/DisagioDigitale/SoapyELAD>). The
FDM-DUO's CAT commands come from the *ELAD FDM-DUO User Manual* v2.6, §6.

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version.
