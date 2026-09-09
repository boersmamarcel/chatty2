pub struct Forecast {
    pub celsius: f32,
}

impl Forecast {
    pub fn for_temperature(celsius: f32) -> Self {
        Self { celsius }
    }

    pub fn describe(&self) -> String {
        match self.celsius {
            t if t < 0.0 => format!("{t:.1}°C — bring a coat, it's freezing"),
            t if t < 18.0 => format!("{t:.1}°C — cool and pleasant"),
            t => format!("{t:.1}°C — warm, stay hydrated"),
        }
    }
}

pub fn parse_temperature(raw: &str) -> Result<f32, String> {
    let trimmed = raw.trim().trim_end_matches(['c', 'C']);
    trimmed.parse().map_err(|_| format!("not a temperature: {raw}"))
}
