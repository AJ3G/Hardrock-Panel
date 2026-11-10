//! TEMPORARY stub until the logbook branch's dxcc.rs is merged.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct Entity {
    pub name: String,
    pub prefix: String,
    pub dxcc: Option<u16>,
    pub continent: &'static str,
    pub cq_zone: u8,
    pub itu_zone: u8,
    pub lat: f64,
    pub lon: f64,
}
pub(crate) fn lookup(_call: &str) -> Option<Entity> {
    None
}
