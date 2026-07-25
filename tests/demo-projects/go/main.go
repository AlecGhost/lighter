package main

import (
	"fmt"

	"example.com/lighter-demo/message"
)

func main() {
	greeter := message.Greeter{Prefix: "Hello"}
	fmt.Println(greeter.Welcome("Ada"))
}
