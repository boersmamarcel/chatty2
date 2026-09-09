mod forecast;

use forecast::Forecast;

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "18C".to_string());
    match forecast::parse_temperature(&arg) {
        Some(celsius) => println!("{}", Forecast::new(celsius).describe()),
        None => eprintln!("could not read a temperature from {arg:?}"),
    }
}
