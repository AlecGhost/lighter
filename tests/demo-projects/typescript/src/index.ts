import { User } from "./models.js";

function welcome(user: User): string {
  return user.greeting("Hello");
}

console.log(welcome(new User("Ada")));
