fn main() {
    if let Some(parsed) =
        clusterdeck_lib::local_dns::responder::parse_cli_args(std::env::args().skip(1))
    {
        let args = match parsed {
            Ok(args) => args,
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
