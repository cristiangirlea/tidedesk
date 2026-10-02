//! Makes the licence signing key and signs licences by hand, until a payment
//! service's webhook does it.
//!
//! ```text
//! cargo run -p tidedesk-core --example licence -- key PRIVATE-KEY-FILE
//! cargo run -p tidedesk-core --example licence -- sign PRIVATE-KEY-FILE "Ana Pop" ana@example.com Pro 2027-10-02 [FEATURE...]
//! ```
//!
//! `key` writes a new private key (never over an existing file) and prints the
//! public key to put in `licence.rs`. `sign` prints the licence block to send;
//! the expiry is a date or `never`. Keep the private key out of the
//! repository.

use anyhow::{Context, Result, bail};
use tidedesk_core::licence::{self, Licence};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("key") if args.len() == 2 => {
            let path = std::path::Path::new(&args[1]);
            if path.exists() {
                bail!(
                    "{} exists already; a new key would void every licence",
                    path.display()
                );
            }
            let (private, public) = licence::new_key()?;
            std::fs::write(path, private).context("saving the private key")?;
            println!(
                "Private key saved to {}. Keep it safe and private.",
                path.display()
            );
            println!("Public key for licence.rs:\n{public}");
        }
        Some("sign") if args.len() >= 6 => {
            let private = std::fs::read(&args[1]).context("reading the private key")?;
            let expires = match args[5].as_str() {
                "never" => None,
                date if tidedesk_core::dates::parse(date).is_some() => Some(date.to_string()),
                other => bail!("{other} is not a date (YYYY-MM-DD) or never"),
            };
            let licence = Licence {
                licensee: args[2].clone(),
                email: args[3].clone(),
                edition: args[4].clone(),
                features: args[6..].to_vec(),
                seats: 1,
                issued: tidedesk_core::dates::today(),
                expires,
            };
            print!("{}", licence::sign(&licence, &private)?);
        }
        _ => bail!(
            "usage: licence key PRIVATE-KEY-FILE | licence sign PRIVATE-KEY-FILE NAME EMAIL EDITION EXPIRES [FEATURE...]"
        ),
    }
    Ok(())
}
