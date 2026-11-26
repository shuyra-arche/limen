use std::vec::Vec;

#[derive(Default)]
pub(super) struct Fixture {
    pub(super) structure: Vec<u8>,
    strings: Vec<u8>,
}

impl Fixture {
    fn token(&mut self, value: u32) {
        self.structure.extend(value.to_be_bytes());
    }

    fn padded(&mut self, bytes: &[u8]) {
        self.structure.extend(bytes);
        self.structure
            .resize(self.structure.len().next_multiple_of(4), 0);
    }

    pub(super) fn node(&mut self, name: &[u8]) {
        self.token(1);
        self.padded(&[name, b"\0"].concat());
    }

    pub(super) fn property(&mut self, name: &[u8], value: &[u8]) {
        self.token(3);
        self.token(value.len().try_into().unwrap());
        self.token(self.strings.len().try_into().unwrap());
        self.strings.extend([name, b"\0"].concat());
        self.padded(value);
    }

    pub(super) fn cells(&mut self, name: &[u8], values: &[u32]) {
        self.property(
            name,
            &values
                .iter()
                .flat_map(|value| value.to_be_bytes())
                .collect::<Vec<_>>(),
        );
    }

    pub(super) fn end(&mut self) {
        self.token(2);
    }

    pub(super) fn root() -> Self {
        let mut fixture = Self::default();
        fixture.node(b"");
        fixture.cells(b"#address-cells", &[1]);
        fixture.cells(b"#size-cells", &[1]);
        fixture
    }

    pub(super) fn finish(mut self) -> Vec<u8> {
        self.end();
        self.token(9);
        let structure = 56;
        let strings = structure + self.structure.len();
        let mut bytes = Vec::new();
        for value in [
            0xd00d_feed,
            strings + self.strings.len(),
            structure,
            strings,
            40,
            17,
            16,
            0,
            self.strings.len(),
            self.structure.len(),
        ] {
            bytes.extend(u32::try_from(value).unwrap().to_be_bytes());
        }
        bytes.extend([0; 16]);
        bytes.extend(self.structure);
        bytes.extend(self.strings);
        bytes
    }
}
