class Account:
    def __init__(self, balance=0):
        self.balance = balance

    def deposit(self, amount):
        self.balance += amount
        return self.balance

    def withdraw(self, amount):
        # bug: overdraft is allowed and negative amounts are accepted
        self.balance -= amount
        return self.balance
