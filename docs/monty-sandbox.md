# Testing MontySandbox

Type this into the chatty2 chat input and send it:

```
Calculate the first 10 fibonacci numbers using Python
```

The assistant will run Python code **without Docker** (no container startup delay).
You should get a response in well under a second containing: `[0, 1, 1, 2, 3, 5, 8, 13, 21, 34]`

Other quick prompts that use the fast path:

```
What is 2 ** 32 in Python?
```
```
Sort [3, 1, 4, 1, 5, 9] with Python and show the result
```
```
Use Python to compute the sum of squares from 1 to 100
```

If you see Docker-backed output (slower, ~200–500 ms), the Monty fast path
is not active — verify that `python3` is installed: `python3 --version`.
