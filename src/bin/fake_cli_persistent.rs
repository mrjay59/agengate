use std::io::{self, BufRead, BufReader, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    
    let mut session_id = String::new();
    let mut keep_alive = false;
    
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--session-id" => {
                if i + 1 < args.len() {
                    session_id = args[i + 1].clone();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            "--keep-alive" => {
                keep_alive = true;
                i += 1;
            }
            _ => i += 1,
        }
    }

    let turn_count = Arc::new(AtomicUsize::new(0));
    let turn_count_clone = turn_count.clone();

    if keep_alive {
        // Persistent mode: keep process alive, read multiple requests from stdin
        let stdin = io::stdin();
        let reader = BufReader::new(stdin.lock());
        
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.trim().is_empty() {
                continue;
            }

            let turn = turn_count_clone.fetch_add(1, Ordering::SeqCst) + 1;
            
            // Simulate some processing delay
            thread::sleep(Duration::from_millis(100));
            
            // Output JSONL event for reasoning (optional)
            println!(r#"{{"type":"reasoning","delta":"persistent turn {turn} for {session_id}"}}"#);
            let _ = io::stdout().flush();
            
            // Output text response
            println!(r#"{{"type":"text","delta":"This is turn {turn} for session {session_id}"}}"#);
            let _ = io::stdout().flush();
            
            // Small delay
            thread::sleep(Duration::from_millis(50));
        }
    } else {
        // Single-turn mode (original behavior)
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        let mut input = String::new();
        reader.read_line(&mut input).ok();
        
        let turn = turn_count.fetch_add(1, Ordering::SeqCst) + 1;
        
        thread::sleep(Duration::from_millis(100));
        println!(r#"{{"type":"text","delta":"Single-turn response {turn} for session {session_id}"}}"#);
        let _ = io::stdout().flush();
        thread::sleep(Duration::from_millis(50));
    }
}