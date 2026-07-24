from dataclasses import dataclass
from enum import Enum


class Priority(Enum):
    LOW = "later"
    HIGH = "urgent"


@dataclass(frozen=True, slots=True)
class Task:
    title: str
    priority: Priority
