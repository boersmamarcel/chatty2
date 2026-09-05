mod forecast;

use forecast::{Forecast, parse_temperature};

fn main() {
    let raw = std::env::args().nth(1).unwrap_or_else(|| "21.5C".to_string());
    match parse_temperature(&raw) {
        Ok(celsius) => {
            let f = Forecast::for_temperature(celsius);
            println!("{}", f.describe());
        }
        Err(e) => eprintln!("error: {e}"),
    }
}
