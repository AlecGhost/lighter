package message

type Greeter struct {
	Prefix string
}

func (greeter Greeter) Welcome(name string) string {
	return greeter.Prefix + ", " + name + "!"
}
