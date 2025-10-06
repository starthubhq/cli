use regex::Regex;

fn main() {
    let steps_re = regex::Regex::new(r"\{\{steps\.([^.]+)\.outputs\[(\d+)\]\.([^}]+)\}\}").unwrap();
    let test_string = "{{steps.create_droplet.outputs[0].body.droplet.id}}";
    
    println!("Testing regex against: {}", test_string);
    
    for cap in steps_re.captures_iter(test_string) {
        println!("Match found!");
        println!("Full match: {}", &cap[0]);
        println!("Step name: {}", &cap[1]);
        println!("Index: {}", &cap[2]);
        println!("JSON path: {}", &cap[3]);
    }
}
