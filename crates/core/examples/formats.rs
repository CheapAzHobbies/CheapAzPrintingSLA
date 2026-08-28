fn main() {
    println!("readable:");
    for i in cheapazsla_core::registry::readable() {
        println!("  {} (.{})", i.name, i.extension);
    }
    println!("writable:");
    for i in cheapazsla_core::registry::writable() {
        println!("  {} (.{})", i.name, i.extension);
    }
}
