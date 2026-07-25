from models import User


def welcome(user: User) -> str:
    return user.greeting("Hello")


print(welcome(User("Ada")))
