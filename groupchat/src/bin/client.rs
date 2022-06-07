use chatlib::{Command, Deserialize, Serialize};
use std::sync::Arc;
use std::{env, io, net::TcpStream, thread};

fn main() -> io::Result<()> {
    let (name, addr) = name_and_addr_from_arg();
    let stream = Arc::new(TcpStream::connect(addr)?);

    {
        let stream = stream.clone();
        thread::spawn(move || {
            while let Ok(cmd) = stream.as_ref().decode() {
                match cmd {
                    Command::Message(msg) => println!("{}", msg),
                    _ => unimplemented!(),
                }
            }
        });
    }

    stream.as_ref().encode(Command::Name(name))?;
    let input = io::stdin();
    let mut input_buf = String::new();
    while input.read_line(&mut input_buf).is_ok() {
        let cmd = command_from_input(&input_buf);
        stream.as_ref().encode(cmd)?;
        input_buf.clear();
    }

    Ok(())
}

fn name_and_addr_from_arg() -> (String, String) {
    let default_name = "Anonymous";
    let default_addr = "127.0.0.1:8000";
    let default_arg = format!("{}@{}", default_name, default_addr);

    let arg_1 = env::args().nth(1).unwrap_or(default_arg);
    let name_addr = arg_1.split('@').collect::<Vec<_>>();

    if name_addr.len() == 1 {
        (String::from(default_name), String::from(name_addr[0]))
    } else if name_addr.len() == 2 {
        let name = if name_addr[0].is_empty() {
            String::from(default_name)
        } else {
            String::from(name_addr[0])
        };

        (name, String::from(name_addr[1]))
    } else {
        panic!("invalid argument");
    }
}

fn command_from_input(input: &str) -> Command {
    let input = input.trim();
    if input.starts_with('/') {
        let cmd = input.split_whitespace().collect::<Vec<_>>();
        match cmd[0] {
            "/chn" => Command::Name(cmd[1].to_string()),
            _ => unimplemented!(),
        }
    } else {
        Command::Message(input.to_string())
    }
}
