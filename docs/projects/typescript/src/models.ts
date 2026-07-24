export enum Priority {
  Low = "later",
  High = "urgent",
}

export interface Task {
  readonly title: string;
  readonly priority: Priority;
}
