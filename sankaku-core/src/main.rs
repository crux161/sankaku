use sankaku::network; // Imports the logic from your new lib.rs

fn main() {
    println!("Starting Sankaku standalone node...");
    network::start_node();
}
