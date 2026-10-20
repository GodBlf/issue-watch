use std::{collections::HashMap, net::IpAddr};

#[derive(Default)]
pub(super) struct Attempts {
    global: Window,
    sources: HashMap<IpAddr, Window>,
}
#[derive(Default)]
struct Window {
    start: i64,
    count: u32,
}
impl Window {
    fn refresh(&mut self, now: i64) {
        if now.saturating_sub(self.start) >= 60 || now < self.start {
            self.start = now;
            self.count = 0;
        }
    }
}
impl Attempts {
    pub(super) fn admit(&mut self, source: IpAddr, now: i64) -> bool {
        self.global.refresh(now);
        self.sources
            .retain(|_, window| now.saturating_sub(window.start) < 60);
        // Rejected global attempts do not grow the source map. A window can admit
        // at most twenty sources, bounding both memory and expensive hash work.
        if self.global.count >= 20 {
            return false;
        }
        let window = self.sources.entry(source).or_default();
        window.refresh(now);
        if window.count >= 5 {
            return false;
        }
        window.count += 1;
        self.global.count += 1;
        true
    }
}
