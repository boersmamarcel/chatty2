/// Strip a trailing `C` and parse what is left as a temperature.
pub fn parse_temperature(input: &str) -> Option<f64> {
    input.trim().trim_end_matches(['C', 'c']).trim().parse().ok()
}

pub struct Forecast {
    celsius: f64,
}

impl Forecast {
    pub fn new(celsius: f64) -> Self {
        Self { celsius }
    }

    pub fn describe(&self) -> String {
        let advice = if self.celsius < 5.0 {
            "cold, wear a coat"
        } else if self.celsius < 20.0 {
            "mild, a jacket will do"
        } else {
            "warm, stay hydrated"
        };
        format!("{:.1}°C — {advice}", self.celsius)
    }
}
