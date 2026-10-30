use std::path::PathBuf;
use xi_client::XiClient;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: basic <path-to-xi-core> <file-to-open>");
        std::process::exit(1);
    }
    let core = PathBuf::from(&args[1]);
    let file = PathBuf::from(&args[2]);

    let client = match XiClient::spawn(&core) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Не удалось запустить xi-core: {e}");
            std::process::exit(1);
        }
    };

    let events = client.events();
    let _req = match client.new_view(Some(file)) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("new_view failed: {e}");
            std::process::exit(1);
        }
    };

    loop {
        match events.recv() {
            Ok(xi_client::Event::ViewOpened { view_id, .. }) => {
                println!("Открыт {view_id}");
                client.insert(view_id, "hello\n");
            }
            Ok(xi_client::Event::DocumentUpdated { view_id }) => {
                if let Some(doc) = client.document(view_id) {
                    println!("--- содержимое {} ---", view_id);
                    for (i, line) in doc.lines.iter().enumerate() {
                        println!("{:>3}: {}", i + 1, line.text);
                    }
                }
                client.shutdown();
                break;
            }
            Ok(xi_client::Event::Alert(msg)) => eprintln!("alert: {msg}"),
            Ok(xi_client::Event::Exited) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }
}
