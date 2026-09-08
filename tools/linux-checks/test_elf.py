import unittest
from elf import inspect


class AbiTests(unittest.TestCase):
    def check(self, *, dynamic=None, versions=None, machine="Advanced Micro Devices X86-64"):
        return inspect(f"Class: ELF64\nMachine: {machine}\n",
                       "[Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]",
                       dynamic or "(NEEDED) Shared library: [libc.so.6]",
                       versions or "Name: GLIBC_2.2.5\nName: GLIBC_2.28",
                       "x86_64-unknown-linux-gnu")

    def test_numeric_versions(self):
        self.assertEqual(self.check()["required_glibc"], "2.28")
        self.assertEqual(self.check(versions="Name: GLIBC_2.9\nName: GLIBC_2.17")["required_glibc"], "2.17")

    def test_rejects_new_private_and_missing_abi_requirements(self):
        for versions in ["Name: GLIBC_2.34", "Name: GLIBC_PRIVATE", "Name: GLIBC_ABI_DT_RELR", "empty"]:
            with self.subTest(versions=versions), self.assertRaises(ValueError):
                self.check(versions=versions)

    def test_rejects_wrong_architecture_extra_libraries_and_rpaths(self):
        with self.assertRaises(ValueError):
            self.check(machine="AArch64")
        for dynamic in ["(NEEDED) Shared library: [libstdc++.so.6]",
                        "(NEEDED) Shared library: [libc.so.6]\n(RUNPATH) Library runpath: [/build/lib]"]:
            with self.subTest(dynamic=dynamic), self.assertRaises(ValueError):
                self.check(dynamic=dynamic)

    def test_arm64_loader(self):
        result = inspect("Class: ELF64\nMachine: AArch64\n",
                         "[Requesting program interpreter: /lib/ld-linux-aarch64.so.1]",
                         "(NEEDED) Shared library: [libc.so.6]", "Name: GLIBC_2.17",
                         "aarch64-unknown-linux-gnu")
        self.assertEqual(result["required_glibc"], "2.17")


if __name__ == '__main__':
    unittest.main()
