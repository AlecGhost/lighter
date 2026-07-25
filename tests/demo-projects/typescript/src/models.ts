export class User {
  constructor(readonly name: string) {}

  greeting(prefix: string): string {
    return `${prefix}, ${this.name}!`;
  }
}
