fn main() {
    if let Some(parsed) =
        clusterdeck_lib::local_dns::responder::parse_cli_args(std::env::args().skip(1))
    {
        let args = match parsed {
            Ok(clusterdeck_lib::local_dns::responder::ResponderCliMode::SelfTest) => {
                println!("{}", clusterdeck_lib::local_dns::responder::SELFTEST_TOKEN);
                return;
            }
            Ok(clusterdeck_lib::local_dns::responder::ResponderCliMode::Run(args)) => args,
            Err(err) => {
                eprintln!("clusterdeck --dns-responder argument error: {err}");
                std::process::exit(1);
            }
        };
        if let Err(err) = clusterdeck_lib::local_dns::responder::run(
            &args.zones_path,
            &args.status_path,
            args.port,
        ) {
            eprintln!("clusterdeck --dns-responder error: {err}");
            std::process::exit(1);
        }
        return;
    }

    clusterdeck_lib::run();
}
