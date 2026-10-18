import pandas as pd

from .helpers import total


class Job:
    def helper(self, frame):
        return frame["price"].sum()

    def run(self, other) -> None:
        a = pd.read_csv("a.csv")
        self.helper(a)
        b = pd.read_csv("b.csv")
        total(b)
        c = pd.read_csv("c.csv")
        other.merge(c, on="k")
