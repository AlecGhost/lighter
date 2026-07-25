class User:
    def __init__(self, name: str) -> None:
        self.name = name

    def greeting(self, prefix: str) -> str:
        return f"{prefix}, {self.name}!"
