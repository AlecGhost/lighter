use lighter_rust_demo::Greeter;

fn main() {
    let greeter = Greeter::new("Hello");
    println!("{}", greeter.greet("Ada"));
}
